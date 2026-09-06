use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write, Seek, SeekFrom, BufWriter};
use std::path::{Path, PathBuf};

const TARGET_SIZE: u64 = 500 * 1024 * 1024; // 目标大小（至少）
const PADDING_PATTERN: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];

fn main() -> io::Result<()> {
    println!("=== DN PAK 文件大小调整工具 ===\n");
    println!("自动规则：");
    println!("  • 文件 < 500 MB → 填充至少至 500 MB");
    println!("  • 文件 ≥ 500 MB 且尾部有填充 → 移除填充");
    println!("  • 文件 ≥ 500 MB 且无填充 → 不处理");
    println!("  • 支持拖入多个文件或文件夹，将批量处理\n");

    loop {
        print!("请拖入 PAK 文件或文件夹（多个用空格分隔），然后按回车：");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        if input == "0" {
            println!("程序退出。");
            break;
        }

        if input.is_empty() {
            continue;
        }

        let raw_paths = parse_paths(input);
        if raw_paths.is_empty() {
            println!("未识别到有效路径，请重试。\n");
            continue;
        }

        let mut pak_files: Vec<PathBuf> = Vec::new();
        for raw in raw_paths {
            let p = Path::new(&raw);
            if !p.exists() {
                println!("路径不存在，跳过: {}", raw);
                continue;
            }
            if p.is_dir() {
                match fs::read_dir(p) {
                    Ok(entries) => {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if path.extension().and_then(|e| e.to_str()) == Some("pak") {
                                pak_files.push(path);
                            }
                        }
                    }
                    Err(e) => println!("读取文件夹失败: {} ({})", raw, e),
                }
            } else if p.extension().and_then(|e| e.to_str()) == Some("pak") {
                pak_files.push(p.to_path_buf());
            } else {
                println!("不是 .pak 文件，跳过: {}", raw);
            }
        }

        if pak_files.is_empty() {
            println!("未找到任何 .pak 文件。\n");
            continue;
        }

        println!("\n共找到 {} 个 .pak 文件，开始处理...", pak_files.len());

        for (idx, file_path) in pak_files.iter().enumerate() {
            println!("\n[{}/{}] 处理: {}", idx + 1, pak_files.len(), file_path.display());
            match process_single_file(file_path) {
                Ok(()) => {}
                Err(e) => println!("  处理失败: {}", e),
            }
        }

        println!("\n✅ 批处理完成！\n");
    }
    Ok(())
}

fn process_single_file(file_path: &Path) -> io::Result<()> {
    let metadata = fs::metadata(file_path)?;
    let current_size = metadata.len();
    let current_mb = current_size as f64 / (1024.0 * 1024.0);
    println!("  当前大小: {} 字节 (≈{:.2} MB)", current_size, current_mb);

    if current_size < TARGET_SIZE {
        println!("  文件小于 500 MB，正在填充 0xDEADBEEF 模式...");
        fill_with_pattern(file_path, TARGET_SIZE)?;
        println!("  ✅ 已填充至至少 500 MB");
    } else {
        let (original_size, padding_len) = detect_pattern_padding(file_path)?;
        if padding_len < 1024 {
            println!("  未检测到明显模式填充，无需处理。");
        } else {
            let removed_mb = padding_len as f64 / (1024.0 * 1024.0);
            let original_mb = original_size as f64 / (1024.0 * 1024.0);
            println!("  检测到模式填充 {} 字节 (≈{:.2} MB)", padding_len, removed_mb);
            println!("  移除填充后大小: {} 字节 (≈{:.2} MB)", original_size, original_mb);
            let file = OpenOptions::new().write(true).open(file_path)?;
            file.set_len(original_size)?;
            println!("  ✅ 已移除填充");
        }
    }
    Ok(())
}

/// 填充文件至至少 target_size，使用 0xDEADBEEF 重复模式，保证写入完整模式
fn fill_with_pattern(file_path: &Path, target_size: u64) -> io::Result<()> {
    let mut file = OpenOptions::new().read(true).write(true).open(file_path)?;
    let current_size = file.metadata()?.len();
    if current_size >= target_size {
        return Ok(());
    }

    file.seek(SeekFrom::Start(current_size))?;
    let mut writer = BufWriter::new(file);
    let remaining = target_size - current_size;
    // 计算需要写入的完整模式数量（向上取整）
    let pattern_count = (remaining + 3) / 4;
    let total_write = pattern_count * 4;

    let mut buffer = [0u8; 4096];
    let mut written = 0u64;
    while written < total_write {
        let chunk = std::cmp::min(buffer.len() as u64, total_write - written) as usize;
        for i in (0..chunk).step_by(4) {
            buffer[i..i + 4].copy_from_slice(&PADDING_PATTERN);
        }
        writer.write_all(&buffer[..chunk])?;
        written += chunk as u64;
    }
    writer.flush()?;
    Ok(())
}

/// 检测文件末尾的 0xDEADBEEF 重复模式，返回（原始大小，填充长度）
fn detect_pattern_padding(path: &Path) -> io::Result<(u64, u64)> {
    let mut file = OpenOptions::new().read(true).open(path)?;
    let file_size = file.metadata()?.len();
    if file_size < 4 {
        return Ok((file_size, 0));
    }

    let mut pos = file_size;
    let mut padding_bytes = 0u64;
    let mut buf = vec![0u8; 4096];

    while pos >= 4 {
        let read_size = std::cmp::min(buf.len() as u64, pos) as usize;
        // 确保读取大小是 4 的倍数，以简化块内检查
        let read_size = (read_size / 4) * 4;
        if read_size == 0 {
            break;
        }

        let seek_pos = pos - read_size as u64;
        file.seek(SeekFrom::Start(seek_pos))?;
        file.read_exact(&mut buf[..read_size])?;

        let mut i = read_size;
        while i >= 4 {
            if &buf[i - 4..i] == &PADDING_PATTERN {
                padding_bytes += 4;
                i -= 4;
            } else {
                let original_size = seek_pos + i as u64;
                return Ok((original_size, padding_bytes));
            }
        }
        // 整个块都是模式，继续向前
        pos = seek_pos;
    }

    // 如果循环结束，说明整个文件都是模式
    Ok((0, file_size))
}

fn parse_paths(input: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in input.chars() {
        if ch == '"' {
            if in_quotes {
                // 结束一个引号内的路径
                if !current.is_empty() {
                    paths.push(current.clone());
                    current.clear();
                }
                in_quotes = false;
            } else {
                // 开始一个引号内的路径
                in_quotes = true;
            }
        } else if in_quotes {
            // 引号内的字符全部加入当前路径
            current.push(ch);
        }
        // 忽略引号外的所有字符（包括空格）
    }

    // 如果用户输入了不带引号的路径（例如手动输入），则将整个输入视为一个路径
    if paths.is_empty() && !input.trim().is_empty() {
        paths.push(input.trim().to_string());
    }

    paths
}
