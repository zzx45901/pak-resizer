use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read, Write, Seek, SeekFrom, Cursor, BufReader, BufWriter, BufRead};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

use aes::Aes256;
use cipher::{BlockDecryptMut, KeyInit};
use cipher::block_padding::Pkcs7;
use cipher::generic_array::GenericArray;
use ecb::Decryptor as EcbDecryptor;
use flate2::read::ZlibDecoder;

type Aes256Ecb = EcbDecryptor<Aes256>;

// 全局状态
static CACHED_KEY: LazyLock<Mutex<Option<Vec<u8>>>> = LazyLock::new(|| Mutex::new(None));
static SUCCESS_KEYS: LazyLock<Mutex<Vec<Vec<u8>>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static VERIFIED_KEYS: LazyLock<Mutex<Vec<Vec<u8>>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static UNVERIFIED_KEYS: LazyLock<Mutex<Vec<Vec<u8>>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static SKIPPED_FILES: LazyLock<Mutex<Vec<String>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static LOG_FILE: LazyLock<Mutex<Option<BufWriter<File>>>> = LazyLock::new(|| Mutex::new(None));

// ========== 日志作用域守卫 ==========
struct LogGuard {
    old: Option<BufWriter<File>>,
}

impl Drop for LogGuard {
    fn drop(&mut self) {
        *LOG_FILE.lock().unwrap() = self.old.take();
    }
}

fn init_logger(prefix: &str) -> LogGuard {
    let _ = fs::create_dir_all("logs");
    let now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let pid = std::process::id();
    let log_name = format!("logs/{}_{}_{}.log", prefix, now, pid);

    let writer = match File::create(&log_name) {
        Ok(f) => BufWriter::new(f),
        Err(e) => {
            eprintln!("无法创建日志文件 {}：{}", log_name, e);
            return LogGuard { old: None };
        }
    };

    let mut lock = LOG_FILE.lock().unwrap();
    let old = lock.take();
    *lock = Some(writer);
    drop(lock);

    let start_msg = format!("========== 运行开始: {:?} ==========", SystemTime::now());
    log_to_file(&start_msg);

    LogGuard { old }
}

fn log_to_file(msg: &str) {
    if let Ok(mut lock) = LOG_FILE.lock() {
        if let Some(ref mut writer) = *lock {
            let _ = writeln!(writer, "{}", msg);
            let _ = writer.flush();
        }
    }
}

fn write_summary_log() {
    let now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let pid = std::process::id();
    let name = format!("logs/summary_{}_{}.log", now, pid);
    let mut file = match File::create(&name) {
        Ok(f) => BufWriter::new(f),
        Err(e) => {
            eprintln!("无法创建总结日志 {}：{}", name, e);
            return;
        }
    };

    // 跳过文件汇总
    if let Ok(skipped) = SKIPPED_FILES.lock() {
        let mut sorted = skipped.clone();
        sorted.sort();
        sorted.dedup();
        if !sorted.is_empty() {
            let _ = writeln!(file, "\n========== 跳过文件总结 ==========");
            for f in &sorted {
                let _ = writeln!(file, "跳过: {}", f);
            }
            let _ = writeln!(file, "=========================================");
        } else {
            let _ = writeln!(file, "\n无跳过文件。");
        }
    }

    // 成功密钥汇总
    if let Ok(succ) = SUCCESS_KEYS.lock() {
        let mut unique = succ.clone();
        unique.sort();
        unique.dedup();
        if !unique.is_empty() {
            let _ = writeln!(file, "\n========== 成功使用的密钥汇总 ==========");
            for (i, key) in unique.iter().enumerate() {
                let hex = hex::encode(key);
                let _ = writeln!(file, "密钥 {}: {}", i + 1, hex);
            }
            let _ = writeln!(file, "=========================================");
        } else {
            let _ = writeln!(file, "\n未收集到任何成功密钥。");
        }
    }

    let _ = writeln!(file, "========== 程序结束 ==========");
}

// 手动已知密钥（可选）
const KNOWN_KEYS: &[&[u8]] = &[];

#[derive(Debug)]
struct Header {
    version: u32,
    file_count: u32,
    file_index_table_offset: u32,
}

#[derive(Debug, Clone)]
struct FileEntry {
    file_path: String,
    raw_size: u32,
    real_size: u32,
    compressed_size: u32,
    file_offset: u32,
    _unk_a: u32,
    _unk_b: [u8; 40],
}

// ---------- 安全输入 ----------
fn prompt(label: &str) -> String {
    print!("{}: ", label);
    let _ = io::stdout().flush();
    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return String::new();
    }
    input.trim().to_string()
}

// ---------- PAK 文件扫描 ----------
fn find_pak_files(dn_path: &str, pattern: Option<&str>) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dn_path) else {
        return Vec::new();
    };
    let mut paks: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()) == Some("pak")
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| pattern.map_or(true, |pat| n.to_lowercase().starts_with(pat)))
                    .unwrap_or(false)
        })
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    paks.sort();
    paks
}

// ---------- 密钥提取（原版 ASCII 扫描，unique>=1）----------
fn extract_keys_from_exe(exe_path: &str) -> Vec<Vec<u8>> {
    let bytes = match std::fs::read(exe_path) {
        Ok(b) => b,
        Err(e) => {
            log_to_file(&format!("[警告] 无法读取 exe: {}", e));
            return Vec::new();
        }
    };

    let mut ascii_keys = Vec::new();
    let mut i = 0;
    while i < bytes.len().saturating_sub(31) {
        let slice = &bytes[i..i + 31];
        if slice.iter().all(|&b| b.is_ascii_alphanumeric()) {
            let next = bytes.get(i + 31).copied().unwrap_or(0);
            if !next.is_ascii_alphanumeric() {
                let unique: HashSet<u8> = slice.iter().copied().collect();
                if unique.len() >= 1 {
                    let mut key = slice.to_vec();
                    key.push(0);
                    ascii_keys.push(key);
                }
            }
        }
        i += 1;
    }
    ascii_keys.sort();
    ascii_keys.dedup();
    log_to_file(&format!("[KEY] 原版 ASCII 扫描找到 {} 个密钥", ascii_keys.len()));
    ascii_keys
}

// ---------- 密钥文件加载 ----------
fn read_keys_from_file(path: &str) -> Vec<Vec<u8>> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            log_to_file(&format!("[警告] 无法打开密钥文件: {}", e));
            return Vec::new();
        }
    };
    let reader = BufReader::new(file);
    let mut keys = Vec::new();
    for (line_num, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                log_to_file(&format!("[警告] 读取第{}行出错: {}", line_num + 1, e));
                continue;
            }
        };
        let hex_str = line.trim();
        if hex_str.is_empty() || hex_str.starts_with('#') {
            continue;
        }
        match hex::decode(hex_str) {
            Ok(bytes) if bytes.len() == 32 => keys.push(bytes),
            Ok(bytes) => {
                log_to_file(&format!(
                    "[警告] 第{}行长度={}字节（应为32），已跳过",
                    line_num + 1,
                    bytes.len()
                ));
            }
            Err(e) => {
                log_to_file(&format!(
                    "[警告] 第{}行解码失败: {}，已跳过",
                    line_num + 1,
                    e
                ));
            }
        }
    }
    log_to_file(&format!("从密钥文件加载了 {} 个密钥", keys.len()));
    keys
}

// ---------- 解密核心 ----------
fn try_decrypt_with_key(encrypted: &[u8], key: &[u8]) -> Option<Vec<u8>> {
    let key = GenericArray::clone_from_slice(key);
    let cipher = Aes256Ecb::new(&key);
    let mut decrypted = encrypted.to_vec();
    if cipher.decrypt_padded_mut::<Pkcs7>(&mut decrypted).is_err() {
        return None;
    }
    let mut decoder = ZlibDecoder::new(Cursor::new(&decrypted));
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).ok()?;
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

// 记录成功密钥到全局列表
fn record_success_key(key: &[u8]) {
    if let Ok(mut succ) = SUCCESS_KEYS.lock() {
        succ.push(key.to_vec());
    }
}

fn decrypt_with_dynamic_keys(encrypted: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    // 1. 缓存密钥
    if let Ok(cache_lock) = CACHED_KEY.lock() {
        if let Some(ref key) = *cache_lock {
            if let Some(data) = try_decrypt_with_key(encrypted, key) {
                log_to_file(&format!("[KEY] 缓存密钥命中: {}", hex::encode(key)));
                record_success_key(key);
                return Some((data, key.clone()));
            }
        }
    } else {
        log_to_file("[ERR] CACHED_KEY 锁中毒");
    }

    // 2. 已验证密钥池
    if let Ok(verified) = VERIFIED_KEYS.lock() {
        for key in verified.iter() {
            if let Some(data) = try_decrypt_with_key(encrypted, key) {
                if let Ok(mut cache) = CACHED_KEY.lock() {
                    *cache = Some(key.clone());
                }
                log_to_file(&format!("[KEY] 已验证密钥命中: {}", hex::encode(key)));
                record_success_key(key);
                return Some((data, key.clone()));
            }
        }
    } else {
        log_to_file("[ERR] VERIFIED_KEYS 锁中毒");
    }

    // 3. 未验证池
    let mut unverified = match UNVERIFIED_KEYS.lock() {
        Ok(lock) => lock,
        Err(_) => {
            log_to_file("[ERR] UNVERIFIED_KEYS 锁中毒");
            return None;
        }
    };

    let mut found_key: Option<Vec<u8>> = None;
    let mut found_data: Option<Vec<u8>> = None;
    let mut new_unverified = Vec::new();

    for key in unverified.iter() {
        if let Some(data) = try_decrypt_with_key(encrypted, key) {
            found_key = Some(key.clone());
            found_data = Some(data);
            break;
        } else {
            new_unverified.push(key.clone());
        }
    }

    if let Some(key) = found_key {
        *unverified = new_unverified;
        if let Ok(mut verified) = VERIFIED_KEYS.lock() {
            verified.push(key.clone());
        }
        if let Ok(mut cache) = CACHED_KEY.lock() {
            *cache = Some(key.clone());
        }
        log_to_file(&format!("[KEY] 从未验证池发现新密钥: {}", hex::encode(&key)));
        record_success_key(&key);
        return Some((found_data.unwrap(), key));
    } else {
        *unverified = new_unverified;
    }

    None
}

// ---------- PAK 解析 ----------
fn parse_header(fs: &mut BufReader<File>) -> io::Result<Header> {
    let mut buf = [0u8; 16];
    fs.read_exact(&mut buf)?;
    let version = u32::from_le_bytes(buf[0..4].try_into().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "slice len"))?);
    let file_count = u32::from_le_bytes(buf[4..8].try_into().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "slice len"))?);
    let file_index_table_offset = u32::from_le_bytes(buf[8..12].try_into().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "slice len"))?);
    Ok(Header { version, file_count, file_index_table_offset })
}

fn parse_file_entries(fs: &mut BufReader<File>, header: &Header) -> io::Result<Vec<FileEntry>> {
    let mut entries = Vec::with_capacity(header.file_count as usize);
    for i in 0..header.file_count {
        fs.seek(SeekFrom::Start(header.file_index_table_offset as u64 + i as u64 * 316))?;
        let mut path_buf = [0u8; 256];
        fs.read_exact(&mut path_buf)?;
        let file_path = String::from_utf8_lossy(&path_buf)
            .trim_end_matches('\0')
            .trim_start_matches('\\')
            .to_string();
        let mut info = [0u8; 60];
        fs.read_exact(&mut info)?;
        entries.push(FileEntry {
            file_path,
            raw_size: u32::from_le_bytes(info[0..4].try_into().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "len"))?),
            real_size: u32::from_le_bytes(info[4..8].try_into().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "len"))?),
            compressed_size: u32::from_le_bytes(info[8..12].try_into().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "len"))?),
            file_offset: u32::from_le_bytes(info[12..16].try_into().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "len"))?),
            _unk_a: u32::from_le_bytes(info[16..20].try_into().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "len"))?),
            _unk_b: info[20..60].try_into().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "len"))?,
        });
    }
    Ok(entries)
}

// ---------- 快速非加密处理 ----------
fn try_unencrypted_decomp(data: &[u8], raw_size: u32, real_size: u32) -> Option<Vec<u8>> {
    if raw_size == real_size {
        Some(data.to_vec())
    } else {
        let mut decoder = ZlibDecoder::new(Cursor::new(data));
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).ok()?;
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }
}

// ---------- 解压单个 PAK ----------
fn pak_extract(input: &str, output: &str, encryption: bool, merge: bool) -> io::Result<()> {
    let pak_stem = Path::new(input)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let pak_name = Path::new(input)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let log_prefix = pak_stem.clone();
    let _guard = init_logger(&log_prefix);
    log_to_file(&format!("=== 开始解压: {} ===", input));

    let file = File::open(input)?;
    let mut fs = BufReader::new(file);
    fs.seek(SeekFrom::Start(256))?;

    let header = parse_header(&mut fs).map_err(|e| {
        log_to_file(&format!("[ERR] 解析文件头失败: {}", e));
        e
    })?;
    let entries = parse_file_entries(&mut fs, &header).map_err(|e| {
        log_to_file(&format!("[ERR] 读取文件条目失败: {}", e));
        e
    })?;

    let mut ok = 0u32;
    let mut skip = 0u32;

    let base_output = if merge {
        PathBuf::from(output)
    } else {
        PathBuf::from(output).join(&pak_stem)
    };

    let cleaned_base: PathBuf = base_output
        .components()
        .filter(|c| !c.as_os_str().to_string_lossy().contains(".pak"))
        .collect();
    let final_base = if cleaned_base.as_os_str().is_empty() {
        &base_output
    } else {
        &cleaned_base
    };

    for entry in &entries {
        let file_path = entry.file_path.split('\0').next().unwrap_or("").to_string();
        log_to_file(&format!("处理: {} (压缩大小: {})", file_path, entry.compressed_size));

        let mut data_file = match File::open(input) {
            Ok(f) => f,
            Err(e) => {
                log_to_file(&format!("  跳过: 无法打开PAK ({})", e));
                if let Ok(mut skipped) = SKIPPED_FILES.lock() {
                    skipped.push(format!("{}\\{}", pak_name, file_path));
                }
                skip += 1;
                continue;
            }
        };
        if data_file.seek(SeekFrom::Start(entry.file_offset as u64)).is_err() {
            log_to_file("  跳过: 偏移错误");
            if let Ok(mut skipped) = SKIPPED_FILES.lock() {
                skipped.push(format!("{}\\{}", pak_name, file_path));
            }
            skip += 1;
            continue;
        }
        let mut compressed = vec![0u8; entry.compressed_size as usize];
        if data_file.read_exact(&mut compressed).is_err() {
            log_to_file("  跳过: 读取数据失败");
            if let Ok(mut skipped) = SKIPPED_FILES.lock() {
                skipped.push(format!("{}\\{}", pak_name, file_path));
            }
            skip += 1;
            continue;
        }

        let mut write_data = None;

        let need_decrypt = encryption
            && !file_path.ends_with(".exe")
            && !file_path.ends_with(".dll")
            && !file_path.contains("xigncode")
            && !file_path.contains("testbranch");

        if need_decrypt && compressed.len() > 16 {
            if let Some(data) = try_unencrypted_decomp(&compressed, entry.raw_size, entry.real_size) {
                write_data = Some(data);
                log_to_file("  未加密处理成功（优先尝试），跳过密钥查找");
                ok += 1;
            } else {
                let encrypted_part = &compressed[16..];
                match decrypt_with_dynamic_keys(encrypted_part) {
                    Some((data, key)) => {
                        // 再次确保记录成功密钥（内部已记录，但双重保险）
                        record_success_key(&key);
                        write_data = Some(data);
                        log_to_file(&format!("  解密成功，密钥: {}", hex::encode(&key)));
                        ok += 1;
                    }
                    None => {
                        log_to_file("  解密失败，且非加密已尝试失败，跳过此文件");
                        if let Ok(mut skipped) = SKIPPED_FILES.lock() {
                            skipped.push(format!("{}\\{}", pak_name, file_path));
                        }
                        skip += 1;
                    }
                }
            }
        } else {
            if let Some(data) = try_unencrypted_decomp(&compressed, entry.raw_size, entry.real_size) {
                write_data = Some(data);
                log_to_file("  非加密处理成功");
                ok += 1;
            } else {
                log_to_file("  非加密处理失败，跳过");
                if let Ok(mut skipped) = SKIPPED_FILES.lock() {
                    skipped.push(format!("{}\\{}", pak_name, file_path));
                }
                skip += 1;
            }
        }

        if let Some(data) = write_data {
            let final_path = final_base.join(&file_path);
            if let Some(parent) = final_path.parent() {
                if let Err(e) = fs::create_dir_all(parent) {
                    log_to_file(&format!("  错误: 创建目录失败: {}", e));
                    continue;
                }
            }
            match File::create(&final_path) {
                Ok(file) => {
                    let mut out_file = BufWriter::new(file);
                    if let Err(e) = out_file.write_all(&data) {
                        log_to_file(&format!("  错误: 写入文件失败: {}", e));
                    } else {
                        log_to_file(&format!("  已写入: {}", final_path.display()));
                    }
                }
                Err(e) => log_to_file(&format!("  错误: 无法创建输出文件: {}", e)),
            }
        }
    }

    // 本 PAK 跳过文件总结
    let prefix = format!("{}\\", pak_name);
    let mut local_skipped: Vec<String> = Vec::new();
    if let Ok(skipped) = SKIPPED_FILES.lock() {
        for item in skipped.iter() {
            if item.starts_with(&prefix) {
                local_skipped.push(item[prefix.len()..].to_string());
            }
        }
    }
    if !local_skipped.is_empty() {
        local_skipped.sort();
        local_skipped.dedup();
        log_to_file("\n========== 本 PAK 跳过文件 ==========");
        for f in &local_skipped {
            log_to_file(&format!("跳过: {}", f));
        }
        log_to_file("=====================================");
    }

    tracing::info!("* 成功: {} / 跳过: {}", ok, skip);
    log_to_file(&format!("=== 解压完成: 成功={}, 跳过={} ===", ok, skip));
    Ok(())
}

// ---------- 主入口 ----------
fn main() {
    let dn_path = prompt("DragonNest 文件夹路径");
    let output_path = prompt("输出文件夹路径");
    let key_file_path = prompt("密钥文件路径（留空跳过）");

    let merge_mode = loop {
        let choice = prompt("解压模式 (m: 合并到同一目录, s: 按PAK文件名分目录) [默认 s]").to_lowercase();
        match choice.as_str() {
            "m" => break true,
            "s" | "" => break false,
            _ => println!("无效输入，请输入 'm' 或 's'"),
        }
    };

    let paks = find_pak_files(&dn_path, None);
    if paks.is_empty() {
        println!("未找到任何 PAK 文件，按回车键退出...");
        let _ = io::stdin().read_line(&mut String::new());
        return;
    }

    let actual_output = if merge_mode {
        let first_pak_stem = Path::new(&paks[0])
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy();
        let merged_dir = format!("{}合并", first_pak_stem);
        let merged_path = PathBuf::from(&output_path).join(&merged_dir);
        merged_path.to_string_lossy().to_string()
    } else {
        output_path.to_string()
    };

    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    tracing::info!("DragonNest PAK Extractor - 原版密钥扫描版");

    // 密钥加载
    if !key_file_path.is_empty() {
        let user_keys = read_keys_from_file(&key_file_path);
        if user_keys.is_empty() {
            tracing::warn!("未能从密钥文件加载任何有效密钥");
        } else {
            VERIFIED_KEYS.lock().ok().map(|mut v| v.extend(user_keys));
        }
    }

    let exe_path = format!(r"{}\DragonNest_x64.exe", dn_path);
    let ascii_keys = extract_keys_from_exe(&exe_path);
    let ascii_key_count = ascii_keys.len();

    if ascii_key_count > 0 {
        VERIFIED_KEYS.lock().ok().map(|mut v| v.extend(ascii_keys));
        tracing::info!("已将 {} 个原版 ASCII 密钥加入密钥池", ascii_key_count);
    } else {
        tracing::warn!("从 exe 未提取到原版 ASCII 密钥");
    }

    let has_verified = VERIFIED_KEYS.lock().map(|v| !v.is_empty()).unwrap_or(false);
    let has_unverified = UNVERIFIED_KEYS.lock().map(|v| !v.is_empty()).unwrap_or(false);
    if !has_verified && !has_unverified {
        tracing::warn!("没有可用的密钥，程序退出。");
    } else {
        tracing::info!("找到 {} 个 PAK 文件", paks.len());
        for pak_path in &paks {
            tracing::info!("正在解压: {}", pak_path);
            if let Err(e) = pak_extract(pak_path, &actual_output, true, merge_mode) {
                tracing::error!("解压 {} 失败: {}", pak_path, e);
            }
        }

        // 输出总结日志
        write_summary_log();

        // 控制台显示结果
        if let Ok(succ) = SUCCESS_KEYS.lock() {
            let mut unique = succ.clone();
            unique.sort();
            unique.dedup();
            if !unique.is_empty() {
                println!("\n========== 成功使用的密钥汇总 ==========");
                for (i, key) in unique.iter().enumerate() {
                    println!("密钥 {}: {}", i + 1, hex::encode(key));
                }
                println!("=========================================");
            } else {
                println!("\n未收集到任何成功密钥。");
            }
        }

        // 额外提示提取到的候选密钥数量
        if let Ok(verified) = VERIFIED_KEYS.lock() {
            let count = verified.len();
            if count > 0 {
                println!("（本次从 exe / 密钥文件共加载 {} 个候选密钥）", count);
            }
        }
    }

    println!("\n按回车键退出...");
    let _ = io::stdin().read_line(&mut String::new());
}
