use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write, Seek, SeekFrom, BufWriter};
use std::path::{Path, PathBuf};

const TARGET_SIZE: u64 = 500 * 1024 * 1024; // 500 MB
const PADDING_PATTERN: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF]; // 魔术填充模式

fn main() -> io::Result<()> {
    println!("=== DN PAK 文件大小调整工具 ===\n");
    println!("自动规则：");
    println!("  • 文件 < 500 MB → 填充到 500 MB");
    println!("  • 文件 ≥ 500 MB 且尾部有该模式填充 → 移除填充");
    println!("  • 文件 ≥ 500 MB 且无该模式填充 → 不处理");
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
        // 放大：使用模式填充到 500 MB
        println!("  文件小于 500 MB，正在使用 0xDEADBEEF 模式填充...");
        fill_with_pattern(file_path, TARGET_SIZE)?;
        println!("   已填充到 500 MB");
    } else {
        // 检测尾部模式填充
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
            println!("   已移除填充");
        }
    }
    Ok(())
}

/// 使用魔术模式填充文件至目标大小
fn fill_with_pattern(file_path: &Path, target_size: u64) -> io::Result<()> {
    let mut file = OpenOptions::new().read(true).write(true).open(file_path)?;
    let current_size = file.metadata()?.len();
    if current_size >= target_size {
        return Ok(());
    }

    file.seek(SeekFrom::Start(current_size))?;
    let mut writer = BufWriter::new(file);
    let mut remaining = target_size - current_size;
    let pattern = PADDING_PATTERN;
    let mut buffer = [0u8; 4096];

    // 先填充完整模式块
    while remaining >= 4 {
        let chunk = std::cmp::min(remaining / 4 * 4, buffer.len() as u64) as usize;
        for i in (0..chunk).step_by(4) {
            buffer[i..i + 4].copy_from_slice(&pattern);
        }
        writer.write_all(&buffer[..chunk])?;
        remaining -= chunk as u64;
    }

    // 处理剩余不足4字节的部分，填充0xDE
    if remaining > 0 {
        let tail = vec![0xDE; remaining as usize];
        writer.write_all(&tail)?;
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

/// 检测文件尾部魔术模式填充，返回（原始大小，填充长度）
fn detect_pattern_padding(path: &Path) -> io::Result<(u64, u64)> {
    let mut file = OpenOptions::new().read(true).open(path)?;
    let file_size = file.metadata()?.len();
    if file_size == 0 {
        return Ok((0, 0));
    }

    const BLOCK_SIZE: usize = 4096;
    let mut buf = vec![0u8; BLOCK_SIZE];
    let mut pos = file_size;
    let mut padding_bytes = 0u64;

    // 辅助函数：检查从缓冲区位置起的4字节是否等于模式
    fn is_pattern(buf: &[u8], idx: usize) -> bool {
        idx + 4 <= buf.len() && buf[idx..idx + 4] == PADDING_PATTERN
    }

    // 先读取最后一个块，处理尾部可能的不完整0xDE
    if pos > 0 {
        let read_size = std::cmp::min(pos as usize, BLOCK_SIZE) as usize;
        let seek_pos = pos - read_size as u64;
        file.seek(SeekFrom::Start(seek_pos))?;
        file.read_exact(&mut buf[..read_size])?;

        let mut i = read_size;
        // 处理末尾连续的0xDE（最多3个，因为如果超过3个，则可能包含完整模式的一部分）
        let mut tail_de = 0;
        while i > 0 && buf[i - 1] == 0xDE && tail_de < 3 {
            tail_de += 1;
            i -= 1;
        }
        padding_bytes += tail_de as u64;

        // 然后匹配完整的模式
        while i >= 4 && is_pattern(&buf[..i], i - 4) {
            padding_bytes += 4;
            i -= 4;
        }

        if i > 0 {
            // 找到边界：非模式字节，原始大小 = seek_pos + i
            let original_size = seek_pos + i as u64;
            return Ok((original_size, padding_bytes));
        }

        // 当前块全部是填充，继续向前
        pos = seek_pos;
    }

    // 继续处理前面的块
    while pos > 0 {
        let read_size = std::cmp::min(pos as usize, BLOCK_SIZE) as usize;
        let seek_pos = pos - read_size as u64;
        file.seek(SeekFrom::Start(seek_pos))?;
        file.read_exact(&mut buf[..read_size])?;

        let mut i = read_size;
        while i >= 4 && is_pattern(&buf[..i], i - 4) {
            padding_bytes += 4;
            i -= 4;
        }

        if i > 0 {
            let original_size = seek_pos + i as u64;
            return Ok((original_size, padding_bytes));
        }
        pos = seek_pos;
    }

    // 整个文件全是填充
    Ok((0, file_size))
}
