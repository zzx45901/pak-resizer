use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write, Seek, SeekFrom, BufWriter};
use std::path::{Path, PathBuf};

const TARGET_SIZE: u64 = 500 * 1024 * 1024; // 目标大小（至少）
const PADDING_PATTERN: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];

fn main() -> io::Result<()> {
    println!("===DN PAK 文件智能大小调整工具v1.2 ===\n");
    println!("自动规则：");
    println!("  • 文件 < 500 MB → 填充至少至 500 MB");
    println!("  • 文件 ≥ 500 MB 且尾部有填充 → 移除填充");
    println!("  • 文件 ≥ 500 MB 且无填充 → 不处理");
    println!("  • 支持拖入多个文件或文件夹，将批量处理\n");

    loop {
        print!("请拖入 PAK 文件或文件夹（可同时拖入多个），然后按回车：");
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

        println!("\n 批处理完成！\n");
    }
    Ok(())
}

fn process_single_file(file_path: &Path) -> io::Result<()> {
    let metadata = fs::metadata(file_path)?;
    let current_size = metadata.len();
    let current_mb = current_size as f64 / (1024.0 * 1024.0);
    println!("  当前大小: {} 字节 (≈{:.2} MB)", current_size, current_mb);

    if current_size < TARGET_SIZE {
        println!("  文件小于 500 MB，正在填充...");
        fill_with_pattern(file_path, TARGET_SIZE)?;
        println!("   已填充至至少 500 MB");
    } else {
        let (original_size, padding_len) = detect_pattern_padding(file_path)?;
        if padding_len == 0 {
            println!("  未检测到模式填充，无需处理。");
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

/// 填充文件至至少 target_size，直接循环写入模式，不要求精确倍数
fn fill_with_pattern(file_path: &Path, target_size: u64) -> io::Result<()> {
    let mut file = OpenOptions::new().read(true).write(true).open(file_path)?;
    let current_size = file.metadata()?.len();
    if current_size >= target_size {
        return Ok(());
    }

    file.seek(SeekFrom::Start(current_size))?;
    let mut writer = BufWriter::new(file);
    let mut remaining = target_size - current_size;

    // 缓冲区大小设为 4 的倍数，但写入时可能多写几个字节也没关系
    let mut buffer = [0u8; 4096];
    // 填充缓冲区，循环写入
    while remaining > 0 {
        let chunk = std::cmp::min(remaining, buffer.len() as u64) as usize;
        // 用模式填充整个 chunk
        for i in (0..chunk).step_by(4) {
            let end = std::cmp::min(i + 4, chunk);
            let copy_len = end - i;
            buffer[i..end].copy_from_slice(&PADDING_PATTERN[..copy_len]);
        }
        writer.write_all(&buffer[..chunk])?;
        remaining -= chunk as u64;
    }
    writer.flush()?;
    Ok(())
}

/// 检测文件末尾的 0xDEADBEEF 重复模式，返回（原始大小，填充长度）
/// 简化逻辑：直接从末尾逐4字节匹配，不检查倍数和阈值
fn detect_pattern_padding(path: &Path) -> io::Result<(u64, u64)> {
    let mut file = OpenOptions::new().read(true).open(path)?;
    let file_size = file.metadata()?.len();
    if file_size < 4 {
        return Ok((file_size, 0));
    }

    let mut pos = file_size;
    let mut padding_bytes = 0u64;
    let mut buf = [0u8; 4];

    while pos >= 4 {
        // 读取当前位置的前4字节（从 pos-4 到 pos）
        file.seek(SeekFrom::Start(pos - 4))?;
        file.read_exact(&mut buf)?;

        if &buf == &PADDING_PATTERN {
            padding_bytes += 4;
            pos -= 4;
        } else {
            // 不匹配，停止
            break;
        }
    }

    // original_size 就是 pos（第一个不匹配的位置）
    Ok((pos, padding_bytes))
}

// ========== 路径解析（支持中文、双引号和盘符/UNC前缀） ==========
fn parse_paths(input: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let bytes = input.as_bytes();

    let mut char_indices = input.char_indices().peekable();
    while let Some((byte_idx, ch)) = char_indices.next() {
        if ch == '"' {
            if in_quotes {
                if !current.is_empty() {
                    paths.push(current.clone());
                    current.clear();
                }
                in_quotes = false;
            } else {
                if !current.is_empty() {
                    let split = split_by_drive_prefix(&current);
                    paths.extend(split);
                    current.clear();
                }
                in_quotes = true;
            }
            continue;
        }

        if !in_quotes {
            // 盘符检测
            if byte_idx + 2 < bytes.len()
                && bytes[byte_idx].is_ascii_alphabetic()
                && bytes[byte_idx + 1] == b':'
                && bytes[byte_idx + 2] == b'\\'
            {
                if !current.is_empty() {
                    paths.push(current.clone());
                    current.clear();
                }
            }
            // UNC 检测
            else if byte_idx + 1 < bytes.len()
                && bytes[byte_idx] == b'\\'
                && bytes[byte_idx + 1] == b'\\'
            {
                if !current.is_empty() {
                    paths.push(current.clone());
                    current.clear();
                }
            }
        }

        current.push(ch);
    }

    if !current.is_empty() {
        if in_quotes {
            paths.push(current);
        } else {
            let split = split_by_drive_prefix(&current);
            paths.extend(split);
        }
    }

    paths.into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

fn split_by_drive_prefix(s: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let bytes = s.as_bytes();

    let mut char_indices = s.char_indices().peekable();
    while let Some((byte_idx, ch)) = char_indices.next() {
        if (byte_idx + 2 < bytes.len()
            && bytes[byte_idx].is_ascii_alphabetic()
            && bytes[byte_idx + 1] == b':'
            && bytes[byte_idx + 2] == b'\\')
            || (byte_idx + 1 < bytes.len()
                && bytes[byte_idx] == b'\\'
                && bytes[byte_idx + 1] == b'\\')
        {
            if !current.is_empty() {
                result.push(current.clone());
                current.clear();
            }
        }
        current.push(ch);
    }

    if !current.is_empty() {
        result.push(current);
    }
    result
}
