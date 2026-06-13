# Dragon Nest PAK Extractor

A fast and reliable **PAK file extractor** for Dragon Nest, written in **Rust**.

This tool is designed to automatically locate and use the correct encryption key by scanning the game executable at runtime, eliminating the need for manual key input or hardcoding.

---

## Features

* **Dynamic Key Extraction**
  Automatically finds the encryption key directly from the game `.exe` file.

* **High Performance**
  Built with Rust for speed, safety, and efficiency.

* **PAK File Extraction**
  Extracts contents from Dragon Nest `.pak` archives.

* **No Manual Setup Required**
  No need to search for or update keys manually.

---

## Getting Started

### Prerequisites

* Dragon Nest game installation
* Rust (if building from source)

### Build

```bash
git clone https://github.com/RifqiSah/dn-pak-rs.git
cd dn-pak-rs
cargo build --release
```

---

## How It Works

1. The tool loads the provided Dragon Nest executable.
2. It scans the binary to **dynamically locate the encryption key**.
3. The key is then used to decrypt and extract the contents of the `.pak` file.

This approach ensures compatibility across different game versions without requiring updates to the extractor.

---

##  Disclaimer

This project is intended for **educational and research purposes only**.

* Do not use this tool for illegal activities.
* Respect the game's terms of service and intellectual property.


---

## Contributing

Contributions are welcome!

Feel free to open issues or submit pull requests to improve functionality, performance, or compatibility.

---

## License

This project is licensed under the MIT License.

---

## Notes

* Compatibility may vary depending on the Dragon Nest version.
* If the key extraction fails, please open an issue with your game version details.

---

Thanks to:
- [DevilProMT](https://github.com/DevilProMT)