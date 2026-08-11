//! Read named REG_DWORDs out of a Windows CE image or a provisioned flash image.
fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "NK.bin".into());
    let raw = std::fs::read(&path).unwrap();
    let wide: Vec<u8> = "AutoFormat".encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
    let mut n = 0;
    let mut at = 0;
    while let Some(i) = raw[at..].windows(wide.len()).position(|w| w == wide) {
        n += 1;
        at += i + 2;
        if at >= raw.len() { break; }
    }
    println!("{path}: {n} textual mentions of AutoFormat");
    for name in ["AutoFormat", "AutoMount", "AutoPart"] {
        match gandalf::registry::find_dword(&raw, name) {
            Some(v) => println!("  {name:12} record {:#010x} data {:#010x} = {:#x}", v.record, v.data, v.value),
            None => println!("  {name:12} not found"),
        }
    }
}
