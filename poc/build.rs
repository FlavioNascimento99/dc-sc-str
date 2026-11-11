use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let ico = out.join("app.ico");
    fs::write(&ico, icon_16()).unwrap();
    let rc = out.join("app.rc");
    let icon_path = ico.display().to_string().replace('\\', "\\\\");
    fs::write(
        &rc,
        format!(
            r#"
1 ICON "{icon_path}"
1 VERSIONINFO
FILEVERSION 0,1,0,0
PRODUCTVERSION 0,1,0,0
FILEFLAGSMASK 0x3f
FILEFLAGS 0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Flavio Nascimento"
      VALUE "FileDescription", "Compartilhamento de tela para chamadas do Discord"
      VALUE "FileVersion", "0.1.0"
      VALUE "ProductName", "Discord Screen Companion"
      VALUE "ProductVersion", "0.1.0"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
        ),
    )
    .unwrap();
    embed_resource::compile(rc, embed_resource::NONE);
}

fn icon_16() -> Vec<u8> {
    let size = 16u32;
    let pixels = size * size;
    let xor_len = pixels * 4;
    let mask_stride = ((size + 31) / 32) * 4;
    let and_len = mask_stride * size;
    let image_offset = 6 + 16;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.push(size as u8);
    bytes.push(size as u8);
    bytes.push(0);
    bytes.push(0);
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&32u16.to_le_bytes());
    bytes.extend_from_slice(&(40 + xor_len + and_len).to_le_bytes());
    bytes.extend_from_slice(&image_offset.to_le_bytes());
    bytes.extend_from_slice(&40u32.to_le_bytes());
    bytes.extend_from_slice(&(size as i32).to_le_bytes());
    bytes.extend_from_slice(&((size as i32) * 2).to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&32u16.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&(xor_len + and_len).to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    for y in (0..size).rev() {
        for x in 0..size {
            let edge = x == 0 || y == 0 || x + 1 == size || y + 1 == size;
            let (b, g, r) = if edge { (90, 70, 40) } else { (170, 110, 40) };
            bytes.extend_from_slice(&[b, g, r, 255]);
        }
    }
    bytes.extend(std::iter::repeat(0).take(and_len as usize));
    bytes
}
