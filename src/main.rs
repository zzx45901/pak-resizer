use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write, Seek, SeekFrom, Cursor, BufReader, BufWriter};
use std::path::{Path, PathBuf};

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use chrono::{Local, Datelike, Timelike};

const TARGET_SIZE: u64 = 500 * 1024 * 1024; // 500 MB
const PADDING_PATTERN: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF]; // 填充模式
const HEADER_MAGIC: &[u8] = b"EyedentityGames Packing File 0.1\0";
const HEADER_OFFSET_FILE_COUNT: u64 = 0x104; // 文件数写入位置
const DATA_START_OFFSET: u64 = 0x104 + 8; // 数据区起始偏移 0x10C

fn main() -> io::Result<()> {
    println!("=== PAK 合并工具 ===\n");
    println!("功能：提取多个 PAK 中的所有文件，合并（后覆盖前）并重新打包为一个 PAK，自动填充到 500 MB。");
    println!("规则：仅支持拖入 .pak 文件，不支持文件夹。\n");

    loop {
        // ---------- 收集输入 .pak 文件 ----------
        let pak_files = loop {
            print!("请拖入多个 .pak 文件，然后按回车：");
            io::stdout().flush()?;
            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            let input = input.trim();

            if input == "0" {
                println!("程序退出。");
                return Ok(());
            }

            if input.is_empty() {
                println!("未输入任何路径，请重试。");
                continue;
            }

            let raw_paths = parse_paths(input);
            let mut files = Vec::new();
            for raw in raw_paths {
                let p = Path::new(&raw);
                if !p.exists() {
                    println!("路径不存在，跳过：{}", raw);
                    continue;
                }
                if p.is_dir() {
                    println!("不支持文件夹，请直接拖入 .pak 文件：{}", raw);
                    continue;
                }
                if p.extension().and_then(|e| e.to_str()) == Some("pak") {
                    files.push(p.to_path_buf());
                } else {
                    println!("不是 .pak 文件，跳过：{}", raw);
                }
            }

            if files.is_empty() {
                println!("未找到任何 .pak 文件，请重新输入。\n");
                continue;
            }

            println!("\n找到 {} 个 PAK 文件，按拖入顺序处理。", files.len());
            for (i, f) in files.iter().enumerate() {
                println!("  {}: {}", i + 1, f.display());
            }
            break files;
        };

        // 读取第一个 PAK 偏移 0x100 处的 4 字节字段（用于写回）
        let mut custom_field: u32 = 0xB0; // 默认值
        if let Some(first_pak) = pak_files.first() {
            if let Ok(mut f) = File::open(first_pak) {
                if f.seek(SeekFrom::Start(0x100)).is_ok() {
                    let mut buf = [0u8; 4];
                    if f.read_exact(&mut buf).is_ok() {
                        custom_field = u32::from_le_bytes(buf);
                    }
                }
            }
        }

        // ---------- 提取并合并（后覆盖前）----------
        println!("\n正在提取并合并文件...");
        let mut merged_files: HashMap<String, Vec<u8>> = HashMap::new();

        for (i, pak_path) in pak_files.iter().enumerate() {
            println!("[{}/{}] 处理 {}", i + 1, pak_files.len(), pak_path.display());
            match extract_files_from_pak(pak_path) {
                Ok(files) => {
                    let count = files.len();
                    for (file_path, data) in files {
                        let prev = merged_files.insert(file_path.clone(), data);
                        if prev.is_some() {
                            println!("  ↻ 覆盖文件：{}", file_path);
                        }
                    }
                    println!("  提取了 {} 个文件", count);
                }
                Err(e) => {
                    println!("  提取失败：{}", e);
                }
            }
        }

        if merged_files.is_empty() {
            println!("\n错误：没有提取到任何文件，无法打包。");
            continue;
        }

        println!("\n合并后共有 {} 个文件。", merged_files.len());

        // ---------- 生成输出文件路径（第一个 PAK 所在目录）----------
        let first_pak_dir = pak_files
            .first()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));

        let output_name = generate_output_name();
        let output_path = first_pak_dir.join(output_name);
        println!("输出文件：{}", output_path.display());

        // ---------- 重新打包 ----------
        println!("\n正在打包...");
        match create_pak(&output_path, &merged_files, custom_field) {
            Ok(()) => {
                println!("打包完成。");
                let metadata = fs::metadata(&output_path)?;
                let size = metadata.len();
                let size_mb = size as f64 / (1024.0 * 1024.0);
                println!("打包后大小：{} 字节 (≈{:.2} MB)", size, size_mb);
                if size < TARGET_SIZE {
                    println!("正在使用 0xDEADBEEF 模式填充到 500 MB ...");
                    fill_file_with_pattern(&output_path, TARGET_SIZE)?;
                    println!(" 填充完成，最终大小至少 500 MB");
                } else {
                    println!("文件已 ≥ 500 MB，无需填充。");
                }
                println!("\n 合并完成！输出文件：{}", output_path.display());
            }
            Err(e) => {
                println!("打包失败：{}", e);
            }
        }

        println!("\n----------------------------------------\n");
    }
}

// ---------- 从单个 PAK 中提取文件（仅非加密解压）----------
fn extract_files_from_pak(pak_path: &Path) -> io::Result<Vec<(String, Vec<u8>)>> {
    let file = File::open(pak_path)?;
    let mut fs = BufReader::new(file);

    // 读取文件头：跳过标识区，在 0x104 处读取 file_count 和 index_table_offset
    fs.seek(SeekFrom::Start(HEADER_OFFSET_FILE_COUNT))?;
    let mut buf = [0u8; 8];
    fs.read_exact(&mut buf)?;
    let file_count = u32::from_le_bytes(buf[0..4].try_into().unwrap());
    let index_table_offset = u32::from_le_bytes(buf[4..8].try_into().unwrap());

    println!("  PAK 信息：文件数={}, 索引偏移={}", file_count, index_table_offset);

    // 读取索引表
    let mut entries = Vec::with_capacity(file_count as usize);
    for i in 0..file_count {
        fs.seek(SeekFrom::Start(index_table_offset as u64 + i as u64 * 316))?;
        let mut path_buf = [0u8; 256];
        fs.read_exact(&mut path_buf)?;
        let file_path = String::from_utf8_lossy(&path_buf)
            .trim_end_matches('\0')
            .to_string();
        let mut info = [0u8; 60];
        fs.read_exact(&mut info)?;
        let compressed_size = u32::from_le_bytes(info[0..4].try_into().unwrap());
        let raw_size = u32::from_le_bytes(info[4..8].try_into().unwrap());
        let file_offset = u32::from_le_bytes(info[12..16].try_into().unwrap());
        entries.push((file_path, raw_size, compressed_size, file_offset));
    }

    // 提取每个文件
    let mut files = Vec::new();
    for (path, raw_size, compressed_size, file_offset) in entries {
        let mut data_file = File::open(pak_path)?;
        data_file.seek(SeekFrom::Start(file_offset as u64))?;
        let mut compressed = vec![0u8; compressed_size as usize];
        data_file.read_exact(&mut compressed)?;

        let data = if raw_size == compressed_size {
            compressed
        } else {
            let mut decoder = ZlibDecoder::new(Cursor::new(&compressed));
            let mut out = Vec::new();
            match decoder.read_to_end(&mut out) {
                Ok(_) if !out.is_empty() => out,
                _ => {
                    eprintln!("  警告：文件 {} 解压失败，使用原始压缩数据", path);
                    compressed
                }
            }
        };
        files.push((path, data));
    }
    Ok(files)
}

// ---------- 打包为新 PAK（无加密，所有文件统一 zlib 压缩）----------
fn create_pak(output_path: &Path, files: &HashMap<String, Vec<u8>>, custom_field: u32) -> io::Result<()> {
    let mut sorted_files: Vec<(&String, &Vec<u8>)> = files.iter().collect();
    sorted_files.sort_by(|a, b| a.0.cmp(b.0));

    let mut file = BufWriter::new(File::create(output_path)?);

    // 写入文件头标识
    file.write_all(HEADER_MAGIC)?;
    // 填充零至偏移 0x100
    let current_pos = HEADER_MAGIC.len() as u64;
    let padding_to_0x100 = 0x100 - current_pos;
    if padding_to_0x100 > 0 {
        file.write_all(&vec![0u8; padding_to_0x100 as usize])?;
    }

    // 写入偏移 0x100 处的 4 字节字段
    file.write_all(&custom_field.to_le_bytes())?;

    // 写入偏移 0x104 处的 8 字节占位（file_count, index_table_offset），之后回填
    file.write_all(&[0u8; 8])?;

    let mut index_entries: Vec<(String, u32, u32, u32, u32, u32, [u8; 40])> = Vec::new();
    let mut current_offset: u64 = DATA_START_OFFSET;

    for (path, data) in sorted_files {
        // 统一进行 zlib 压缩
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data)?;
        let compressed = encoder.finish()?;

        let raw_size = data.len() as u32;
        let compressed_size = compressed.len() as u32;

        // 写入文件数据
        file.write_all(&compressed)?;

        // 索引信息：路径，压缩后大小，原始大小，第三大小（设为压缩后大小），偏移，未知(0)，40字节填充
        let path_with_slash = if path.starts_with('\\') {
            path.clone()
        } else {
            format!("\\{}", path)
        };
        index_entries.push((
            path_with_slash,
            compressed_size,
            raw_size,
            compressed_size,
            current_offset as u32,
            0,
            [0u8; 40],
        ));

        current_offset += compressed_size as u64;
    }

    // 写入索引表
    let index_table_offset = current_offset;
    for (path, zsize, size, zsize1, offset, unk3, padding) in &index_entries {
        // 写入 256 字节路径
        let mut path_buf = [0u8; 256];
        let bytes = path.as_bytes();
        let len = bytes.len().min(256);
        path_buf[..len].copy_from_slice(&bytes[..len]);
        file.write_all(&path_buf)?;

        // 写入 60 字节元信息
        let mut info = [0u8; 60];
        info[0..4].copy_from_slice(&zsize.to_le_bytes());
        info[4..8].copy_from_slice(&size.to_le_bytes());
        info[8..12].copy_from_slice(&zsize1.to_le_bytes());
        info[12..16].copy_from_slice(&offset.to_le_bytes());
        info[16..20].copy_from_slice(&unk3.to_le_bytes());
        info[20..60].copy_from_slice(padding);
        file.write_all(&info)?;
    }

    // 回填头部：file_count 和 index_table_offset
    file.flush()?;
    let mut file_mut = OpenOptions::new().write(true).open(output_path)?;
    file_mut.seek(SeekFrom::Start(HEADER_OFFSET_FILE_COUNT))?;
    file_mut.write_all(&(index_entries.len() as u32).to_le_bytes())?; // file_count
    file_mut.write_all(&(index_table_offset as u32).to_le_bytes())?;  // index_table_offset

    Ok(())
}

// ---------- 使用 0xDEADBEEF 模式填充文件至至少 target_size ----------
fn fill_file_with_pattern(file_path: &Path, target_size: u64) -> io::Result<()> {
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
        // 确保 chunk 是 4 的倍数
        let chunk = (chunk / 4) * 4;
        writer.write_all(&buffer[..chunk])?;
        remaining -= chunk as u64;
    }

    // 如果剩余 1-3 字节，写入完整模式（多写几个字节，不影响）
    if remaining > 0 {
        writer.write_all(&PADDING_PATTERN)?;
    }

    writer.flush()?;
    Ok(())
}

// ---------- 生成输出文件名：0dnResource00-合并YYMMDDHHMMSS.pak ----------
fn generate_output_name() -> String {
    let now = Local::now();
    format!(
        "0dnResource00-合并{:02}{:02}{:02}{:02}{:02}{:02}.pak",
        now.year() % 100,
        now.month(),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}

// ---------- 路径解析：支持中文、英文双引号、盘符/UNC前缀，不使用空格 ----------
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
