use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const TARGET_SIZE: u64 = 500 * 1024 * 1024; // 500 MB

fn main() -> io::Result<()> {
    println!("=== DN PAK 文件大小调整工具 ===\n");
    println!("自动规则：");
    println!("  • 文件 < 500 MB → 填充到 500 MB");
    println!("  • 文件 ≥ 500 MB 且尾部有填充 → 移除填充");
    println!("  • 文件 ≥ 500 MB 且无填充 → 不处理");
    println!("  • 支持拖入多个文件或文件夹，将批量处理\n");

    loop {
        print!("请拖入 PAK 文件或文件夹（多个用空格分隔，输入 0 退出），然后按回车：");
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
        // 放大
        println!("  文件小于 500 MB，正在填充...");
        let file = OpenOptions::new().write(true).open(file_path)?;
        file.set_len(TARGET_SIZE)?;
        println!("  已放大到 500 MB");
    } else {
        // 检测尾部填充
        let (original_size, padding_len) = detect_padding(file_path)?;
        if padding_len < 1024 {
            println!("  未检测到明显填充，无需处理。");
        } else {
            let removed_mb = padding_len as f64 / (1024.0 * 1024.0);
            let original_mb = original_size as f64 / (1024.0 * 1024.0);
            println!("  检测到填充 {} 字节 (≈{:.2} MB)", padding_len, removed_mb);
            println!("  移除填充后大小: {} 字节 (≈{:.2} MB)", original_size, original_mb);
            let file = OpenOptions::new().write(true).open(file_path)?;
            file.set_len(original_size)?;
            println!("  已移除填充");
        }
    }
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
                in_quotes = !in_quotes; // 切换引号状态
            }
            ' ' if !in_quotes => {
                // 空格分隔（不在引号内）
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

/// 检测文件尾部连续零字节填充，返回（原始大小，填充长度）
fn detect_padding(path: &Path) -> io::Result<(u64, u64)> {
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

        let mut zeros_in_block = 0;
        for i in (0..read_size).rev() {
            if buf[i] == 0 {
                zeros_in_block += 1;
            } else {
                padding_bytes += zeros_in_block;
                let zeros_in_block_usize = zeros_in_block as usize;
                let original_size = seek_pos + (read_size - zeros_in_block_usize) as u64;
                return Ok((original_size, padding_bytes));
            }
        }
        padding_bytes += read_size as u64;
        pos = seek_pos;
    }

    Ok((0, file_size))
}
