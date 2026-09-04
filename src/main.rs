use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write, Seek, SeekFrom, Cursor, BufReader, BufWriter};
use std::path::{Path, PathBuf};

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use chrono::{Local, Datelike, Timelike};

const TARGET_SIZE: u64 = 500 * 1024 * 1024; // 500 MB
const HEADER_MAGIC: &[u8] = b"EyedentityGames Packing File 0.1\0";
const HEADER_OFFSET_FILE_COUNT: u64 = 0x104; // 文件数写入位置
const DATA_START_OFFSET: u64 = 0x104 + 8; // 数据区起始偏移 0x10C

fn main() -> io::Result<()> {
    println!("=== PAK 合并工具（无加密）===\n");
    println!("功能：提取多个 PAK 中的所有文件，合并（后覆盖前）并重新打包为一个 PAK，自动填充到 500 MB。");
    println!("规则：仅支持拖入 .pak 文件，不支持文件夹。\n");

    // ---------- 收集输入 .pak 文件 ----------
    let pak_files = loop {
        print!("请拖入多个 .pak 文件（用空格分隔），然后按回车：");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

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
        return Ok(());
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
                println!("正在填充到 500 MB ...");
                let file = OpenOptions::new().write(true).open(&output_path)?;
                file.set_len(TARGET_SIZE)?;
                println!("✅ 填充完成，最终大小：500 MB");
            } else {
                println!("文件已 ≥ 500 MB，无需填充。");
            }
            println!("\n✅ 合并完成！输出文件：{}", output_path.display());
        }
        Err(e) => {
            println!("打包失败：{}", e);
        }
    }

    println!("按回车键退出...");
    let _ = io::stdin().read_line(&mut String::new());
    Ok(())
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

// ---------- 解析输入字符串（支持带引号路径）----------
fn parse_paths(input: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in input.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
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
