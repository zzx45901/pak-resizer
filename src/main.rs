use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write, Seek, SeekFrom, BufWriter};
use std::path::{Path, PathBuf};

const TARGET_SIZE: u64 = 500 * 1024 * 1024; // 500 MB
const PADDING_BYTE: u8 = 0x2E;             // 填充字符：'.'

fn main() -> io::Result<()> {
    println!("===  DN PAK 大小调整工具 ===\n");
    println!("自动规则：");
    println!("  • 文件 < 500 MB → 填充到 500 MB（使用 '.' 填充）");
    println!("  • 文件 ≥ 500 MB 且尾部有 '.' 填充 → 移除填充");
    println!("  • 文件 ≥ 500 MB 且无 '.' 填充 → 不处理");
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

        // 解析输入，得到所有目标路径（文件或文件夹）
        let raw_paths = parse_paths(input);
        if raw_paths.is_empty() {
            println!("未识别到有效路径，请重试。\n");
            continue;
        }

        // 收集所有需要处理的 .pak 文件
        let mut pak_files: Vec<PathBuf> = Vec::new();
        for raw in raw_paths {
            let p = Path::new(&raw);
            if !p.exists() {
                println!("路径不存在，跳过: {}", raw);
                continue;
            }

            if p.is_dir() {
                // 扫描文件夹下的 .pak 文件（非递归）
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

        // 依次处理每个文件
        for (idx, file_path) in pak_files.iter().enumerate() {
            println!("\n[{}/{}] 处理: {}", idx + 1, pak_files.len(), file_path.display());
            match process_single_file(file_path) {
                Ok(()) => {}
                Err(e) => println!("  处理失败: {}", e),
            }
        }

        println!("\n 批处理完成！\n");
    }
    Ok(())
}

/// 处理单个 PAK 文件（智能判断放大/缩小）
fn process_single_file(file_path: &Path) -> io::Result<()> {
    let metadata = fs::metadata(file_path)?;
    let current_size = metadata.len();
    let current_mb = current_size as f64 / (1024.0 * 1024.0);
    println!("  当前大小: {} 字节 (≈{:.2} MB)", current_size, current_mb);

    if current_size < TARGET_SIZE {
        // 放大：使用 '.' 填充到 500 MB
        println!("  文件小于 500 MB，正在使用 '.' 填充...");
        fill_with_dot(file_path, TARGET_SIZE)?;
        println!("   已填充到 500 MB");
    } else {
        // 检测尾部 '.' 填充
        let (original_size, padding_len) = detect_dot_padding(file_path)?;
        if padding_len < 1024 {
            println!("  未检测到明显 '.' 填充，无需处理。");
        } else {
            let removed_mb = padding_len as f64 / (1024.0 * 1024.0);
            let original_mb = original_size as f64 / (1024.0 * 1024.0);
            println!("  检测到 '.' 填充 {} 字节 (≈{:.2} MB)", padding_len, removed_mb);
            println!("  移除填充后大小: {} 字节 (≈{:.2} MB)", original_size, original_mb);
            let file = OpenOptions::new().write(true).open(file_path)?;
            file.set_len(original_size)?;
            println!("   已移除填充");
        }
    }
    Ok(())
}

/// 使用特定字符填充文件至目标大小
fn fill_with_dot(file_path: &Path, target_size: u64) -> io::Result<()> {
    let mut file = OpenOptions::new().read(true).write(true).open(file_path)?;
    let current_size = file.metadata()?.len();
    if current_size >= target_size {
        return Ok(());
    }

    file.seek(SeekFrom::Start(current_size))?;
    let mut writer = BufWriter::new(file);
    let mut remaining = target_size - current_size;
    let mut buffer = [PADDING_BYTE; 4096];

    while remaining > 0 {
        let chunk = std::cmp::min(remaining, buffer.len() as u64) as usize;
        writer.write_all(&buffer[..chunk])?;
        remaining -= chunk as u64;
    }
    writer.flush()?;
    Ok(())
}

/// 解析输入字符串，支持带引号的路径（Windows 拖入多个文件时会自动加引号）
fn parse_paths(input: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in input.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
            }
            ' ' if !in_quotes => {
                if !current.is_empty() {
                    paths.push(current.clone());
                    current.clear();
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        paths.push(current);
    }
    paths
}

/// 检测文件尾部连续 '.' (0x2E) 填充，返回（原始大小，填充长度）
fn detect_dot_padding(path: &Path) -> io::Result<(u64, u64)> {
    let mut file = OpenOptions::new().read(true).open(path)?;
    let file_size = file.metadata()?.len();
    if file_size == 0 {
        return Ok((0, 0));
    }

    let mut buf = vec![0u8; 4096];
    let mut pos = file_size;
    let mut padding_bytes = 0u64;

    while pos > 0 {
        let read_size = std::cmp::min(buf.len() as u64, pos) as usize;
        let seek_pos = pos - read_size as u64;
        file.seek(SeekFrom::Start(seek_pos))?;
        file.read_exact(&mut buf[..read_size])?;

        let mut dots_in_block = 0;
        for i in (0..read_size).rev() {
            if buf[i] == PADDING_BYTE {
                dots_in_block += 1;
            } else {
                padding_bytes += dots_in_block;
                let dots_in_block_usize = dots_in_block as usize;
                let original_size = seek_pos + (read_size - dots_in_block_usize) as u64;
                return Ok((original_size, padding_bytes));
            }
        }
        // 整个块都是 '.'，继续向前
        padding_bytes += read_size as u64;
        pos = seek_pos;
    }

    // 整个文件全是 '.'
    Ok((0, file_size))
}
