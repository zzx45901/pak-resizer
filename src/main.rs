use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write, Seek, SeekFrom, BufWriter};
use std::path::{Path, PathBuf};

const TARGET_SIZE: u64 = 500 * 1024 * 1024; // 500 MB
const PADDING_PATTERN: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];
const SCAN_BLOCK_SIZE: usize = 64 * 1024; // 64 KB

fn main() -> io::Result<()> {
    println!("=== DN PAK 文件大小调整工具 ===\n");
    println!("自动规则：");
    println!("  • 文件 < 500 MB → 填充至至少 500 MB");
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

/// 填充文件至至少 target_size，只写入完整的 4 字节模式，不添加额外 0x00
fn fill_with_pattern(file_path: &Path, target_size: u64) -> io::Result<()> {
    let mut file = OpenOptions::new().read(true).write(true).open(file_path)?;
    let current_size = file.metadata()?.len();
    if current_size >= target_size {
        return Ok(());
    }

    file.seek(SeekFrom::Start(current_size))?;
    let mut writer = BufWriter::with_capacity(64 * 1024, file);
    let mut remaining = target_size - current_size;

    // 64KB 缓冲区，预填充模式
    let mut buffer = [0u8; 64 * 1024];
    for i in (0..buffer.len()).step_by(4) {
        buffer[i..i + 4].copy_from_slice(&PADDING_PATTERN);
    }

    while remaining >= 4 {
        let chunk = std::cmp::min(remaining, buffer.len() as u64) as usize;
        // 确保 chunk 是 4 的倍数，但 remaining 本来就是4的倍数（target_size 是4的倍数）
        let chunk = (chunk / 4) * 4;
        writer.write_all(&buffer[..chunk])?;
        remaining -= chunk as u64;
    }

    // 理论上 remaining 此时为 0，因为 target_size 是 4 的倍数，且 remaining 初始也是4的倍数
    // 但如果由于某些原因 remaining 不为 0（例如目标大小不是4的倍数），则直接写入完整模式超一点
    if remaining > 0 {
        writer.write_all(&PADDING_PATTERN)?;
    }

    writer.flush()?;
    Ok(())
}

/// 检测文件末尾的 0xDEADBEEF 重复模式，返回（原始大小，填充长度）
/// 使用大缓冲区扫描，不要求文件大小为4的倍数
fn detect_pattern_padding(path: &Path) -> io::Result<(u64, u64)> {
    let mut file = OpenOptions::new().read(true).open(path)?;
    let file_size = file.metadata()?.len();
    if file_size < 4 {
        return Ok((file_size, 0));
    }

    let mut pos = file_size;
    let mut padding_bytes = 0u64;
    let mut buf = vec![0u8; SCAN_BLOCK_SIZE];

    while pos >= 4 {
        let read_start = pos.saturating_sub(SCAN_BLOCK_SIZE as u64);
        let read_len = (pos - read_start) as usize;
        file.seek(SeekFrom::Start(read_start))?;
        file.read_exact(&mut buf[..read_len])?;

        // 从缓冲区末尾向前检查，步长4
        let mut i = read_len;
        while i >= 4 {
            if &buf[i - 4..i] == &PADDING_PATTERN {
                padding_bytes += 4;
                i -= 4;
            } else {
                let original_size = read_start + i as u64;
                return Ok((original_size, padding_bytes));
            }
        }

        // 整个缓冲区都是模式，继续向前
        pos = read_start;
    }

    // 文件全部是模式
    Ok((0, file_size))
}

// ========== 路径解析（支持中文、英文双引号和盘符/UNC前缀） ==========
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
