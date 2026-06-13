use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write, Seek, SeekFrom};
use std::path::Path;

fn main() -> io::Result<()> {
    println!("=== PAK 文件大小调整工具 ===\n");

    loop {
        // 1. 选择模式（新增退出选项）
        let mode = loop {
            println!("请选择操作：");
            println!("  1. 放大（填充到 500 MB）");
            println!("  2. 缩小（自动移除尾部填充）");
            println!("  0. 退出");
            print!("输入数字 (1/2/0): ");
            io::stdout().flush()?;
            let mut choice = String::new();
            io::stdin().read_line(&mut choice)?;
            match choice.trim() {
                "1" => break "enlarge",
                "2" => break "shrink",
                "0" => return Ok(()), // 直接退出
                _ => println!("无效输入，请重新输入。"),
            }
        };

        // 2. 拖入文件
        print!("\n请将 PAK 文件拖入此窗口，然后按回车：");
        io::stdout().flush()?;
        let mut path_input = String::new();
        io::stdin().read_line(&mut path_input)?;
        let path = path_input.trim().trim_matches('"');
        if path.is_empty() || !Path::new(path).exists() {
            eprintln!("错误：文件不存在。\n");
            continue; // 返回主菜单
        }

        let metadata = match fs::metadata(path) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("读取文件信息失败: {}\n", e);
                continue;
            }
        };
        let current_size = metadata.len();
        let current_mb = current_size as f64 / (1024.0 * 1024.0);
        println!("\n当前文件大小: {} 字节 (≈{:.2} MB)", current_size, current_mb);

        // 3. 执行操作
        match mode {
            "enlarge" => {
                let target_mb = 500u64;
                let target_size = target_mb * 1024 * 1024;
                if current_size >= target_size {
                    println!("文件已经 ≥ {} MB，无需放大。\n", target_mb);
                    continue;
                }

                println!("正在放大到 {} MB ...", target_mb);
                let file = match OpenOptions::new().write(true).open(path) {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("打开文件失败: {}\n", e);
                        continue;
                    }
                };
                if let Err(e) = file.set_len(target_size) {
                    eprintln!("操作失败: {}\n", e);
                } else {
                    println!("✅ 放大完成！文件大小: {} MB\n", target_mb);
                }
            }

            "shrink" => {
                let (original_size, padding_len) = match detect_padding(path) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("检测填充失败: {}\n", e);
                        continue;
                    }
                };

                if padding_len < 1024 {
                    println!("\n未检测到明显填充（尾部连续零字节不足 1 KB），文件无需缩小。\n");
                    continue;
                }

                let removed_mb = padding_len as f64 / (1024.0 * 1024.0);
                let original_mb = original_size as f64 / (1024.0 * 1024.0);
                println!("\n检测到填充 {} 字节 (≈{:.2} MB)", padding_len, removed_mb);
                println!("移除填充后文件大小: {} 字节 (≈{:.2} MB)", original_size, original_mb);

                let file = match OpenOptions::new().write(true).open(path) {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("打开文件失败: {}\n", e);
                        continue;
                    }
                };
                if let Err(e) = file.set_len(original_size) {
                    eprintln!("操作失败: {}\n", e);
                } else {
                    println!("✅ 缩小完成！\n");
                }
            }
            _ => unreachable!(),
        }
    }
}

fn detect_padding(path: &str) -> io::Result<(u64, u64)> {
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
                let original_size = seek_pos + (read_size - zeros_in_block) as u64;
                return Ok((original_size, padding_bytes));
            }
        }
        padding_bytes += read_size as u64;
        pos = seek_pos;
    }

    Ok((0, file_size))
}
