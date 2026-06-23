use std::str;

use cull_core::DecodedSourceInfo;
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedSource {
    pub text: String,
    pub info: DecodedSourceInfo,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum SourceDecodeError {
    #[error("source declares unsupported encoding `{0}`")]
    UnsupportedEncoding(String),
    #[error("source is not valid {encoding}: {message}")]
    InvalidEncoding { encoding: String, message: String },
    #[error("UTF-8 BOM cannot be combined with declared encoding `{0}`")]
    BomWithNonUtf8Encoding(String),
}

pub fn decode_python_source(bytes: &[u8]) -> Result<DecodedSource, SourceDecodeError> {
    let had_utf8_bom = bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
    let bytes_without_bom = if had_utf8_bom { &bytes[3..] } else { bytes };
    let declared = find_declared_encoding(bytes_without_bom);
    let encoding = declared.unwrap_or_else(|| "utf-8".to_owned());
    let normalized = normalize_encoding(&encoding);

    if had_utf8_bom && !matches!(normalized.as_str(), "utf-8" | "utf8" | "utf-8-sig") {
        return Err(SourceDecodeError::BomWithNonUtf8Encoding(encoding));
    }

    let text = match normalized.as_str() {
        "utf-8" | "utf8" | "utf-8-sig" => decode_utf8(bytes_without_bom, &encoding)?,
        "ascii" | "us-ascii" => decode_ascii(bytes_without_bom, &encoding)?,
        "latin-1" | "latin1" | "iso-8859-1" | "iso8859-1" => decode_latin1(bytes_without_bom),
        _ => return Err(SourceDecodeError::UnsupportedEncoding(encoding)),
    };

    Ok(DecodedSource {
        text,
        info: DecodedSourceInfo {
            encoding,
            had_utf8_bom,
        },
    })
}

fn decode_utf8(bytes: &[u8], encoding: &str) -> Result<String, SourceDecodeError> {
    str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|error| SourceDecodeError::InvalidEncoding {
            encoding: encoding.to_owned(),
            message: error.to_string(),
        })
}

fn decode_ascii(bytes: &[u8], encoding: &str) -> Result<String, SourceDecodeError> {
    if let Some((index, byte)) = bytes
        .iter()
        .copied()
        .enumerate()
        .find(|(_, byte)| !byte.is_ascii())
    {
        return Err(SourceDecodeError::InvalidEncoding {
            encoding: encoding.to_owned(),
            message: format!("non-ASCII byte 0x{byte:02x} at offset {index}"),
        });
    }
    Ok(String::from_utf8(bytes.to_vec()).expect("ASCII bytes are valid UTF-8"))
}

fn decode_latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| char::from(*byte)).collect()
}

fn normalize_encoding(encoding: &str) -> String {
    encoding.trim().to_ascii_lowercase().replace('_', "-")
}

fn find_declared_encoding(bytes: &[u8]) -> Option<String> {
    first_two_lines(bytes)
        .into_iter()
        .find_map(extract_encoding_from_line)
}

fn first_two_lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            lines.push(&bytes[start..index]);
            start = index + 1;
            if lines.len() == 2 {
                return lines;
            }
        }
    }
    if start <= bytes.len() && lines.len() < 2 {
        lines.push(&bytes[start..]);
    }
    lines
}

fn extract_encoding_from_line(line: &[u8]) -> Option<String> {
    let line = trim_cr(line);
    let first_non_ws = line
        .iter()
        .position(|byte| !matches!(*byte, b' ' | b'\t' | 0x0c))?;
    if line[first_non_ws] != b'#' {
        return None;
    }

    let line = String::from_utf8_lossy(line);
    let bytes = line.as_bytes();
    let coding = find_ascii_subslice(bytes, b"coding")?;
    let after = bytes.get(coding + b"coding".len()..)?;
    let delimiter = after.iter().position(|byte| matches!(*byte, b':' | b'='))?;
    let mut label_start = coding + b"coding".len() + delimiter + 1;
    while matches!(bytes.get(label_start), Some(b' ' | b'\t')) {
        label_start += 1;
    }
    let label_end = bytes[label_start..]
        .iter()
        .position(|byte| !is_encoding_label_byte(*byte))
        .map(|offset| label_start + offset)
        .unwrap_or(bytes.len());

    (label_start < label_end).then(|| line[label_start..label_end].to_owned())
}

fn trim_cr(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}

fn is_encoding_label_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
}

fn find_ascii_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_default_utf8() {
        let decoded = decode_python_source("x = 'é'\n".as_bytes()).unwrap();
        assert_eq!(decoded.text, "x = 'é'\n");
        assert_eq!(decoded.info.encoding, "utf-8");
    }

    #[test]
    fn decodes_latin1_cookie() {
        let decoded = decode_python_source(b"# coding: latin-1\nname = '\xe9'\n").unwrap();
        assert_eq!(decoded.text, "# coding: latin-1\nname = 'é'\n");
        assert_eq!(decoded.info.encoding, "latin-1");
    }

    #[test]
    fn strips_utf8_bom() {
        let decoded = decode_python_source(b"\xef\xbb\xbf# coding: utf-8\nx = 1\n").unwrap();
        assert_eq!(decoded.text, "# coding: utf-8\nx = 1\n");
        assert!(decoded.info.had_utf8_bom);
    }

    #[test]
    fn rejects_invalid_ascii() {
        let error = decode_python_source(b"# coding: ascii\nx = '\xe9'\n").unwrap_err();
        assert!(matches!(error, SourceDecodeError::InvalidEncoding { .. }));
    }
}
