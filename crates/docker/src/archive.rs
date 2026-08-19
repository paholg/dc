use crate::client::Docker;
use crate::error::{Result, TarFileTooLargeSnafu, TarNameTooLongSnafu};
use crate::request_ext::ReqwestExt;

impl Docker {
    /// `PUT /containers/{id}/archive?path=<dest>` — extract a tar archive into
    /// `dest` inside the container.
    pub async fn upload_archive(&self, id: &str, dest_dir: &str, tar: Vec<u8>) -> Result<()> {
        let mut url = self.url(["containers", id, "archive"])?;
        url.query_pairs_mut().append_pair("path", dest_dir);
        self.http()
            .put(url)
            .header("Content-Type", "application/x-tar")
            .body(tar)
            .try_send_empty()
            .await
    }
}

/// Longest entry name a ustar header can hold. Longer names need a `PAX` or
/// GNU extension header, which this builder doesn't write.
const MAX_NAME: usize = 100;

/// Largest file the 12-byte ustar size field can describe: 11 octal digits and
/// a NUL, so 8 GiB - 1.
const MAX_SIZE: u64 = (1 << 33) - 1;

/// Build a tar archive containing exactly one regular file.
///
/// `filename` is stored as the entry name (no path components). `mtime` is set
/// to 0; `mode` is `0o644`. The output is a complete archive including the two
/// trailing zero blocks tar(1) expects as an end-of-archive marker.
pub fn build_single_file_tar(filename: &str, content: &[u8]) -> Result<Vec<u8>> {
    build_archive(&[(filename, content)])
}

/// Build a tar archive containing every `(filename, content)` entry, in order.
/// Each entry is a regular 0o644 file. Output includes the two trailing zero
/// blocks tar(1) expects.
///
/// Errors if an entry cannot be described by a ustar header: a name over 100
/// bytes, or a file of 8 GiB or more.
pub fn build_archive(files: &[(&str, &[u8])]) -> Result<Vec<u8>> {
    let body_size: usize = files.iter().map(|(_, c)| 512 + round_up_512(c.len())).sum();
    let mut out = Vec::with_capacity(body_size + 1024);
    for (name, content) in files {
        out.extend_from_slice(&ustar_header(name, content.len())?);
        out.extend_from_slice(content);
        let pad = round_up_512(content.len()) - content.len();
        out.extend(std::iter::repeat_n(0, pad));
    }
    out.extend(std::iter::repeat_n(0, 1024));
    Ok(out)
}

fn round_up_512(n: usize) -> usize {
    (n + 511) & !511
}

fn ustar_header(filename: &str, size: usize) -> Result<[u8; 512]> {
    let mut h = [0u8; 512];

    let name = filename.as_bytes();
    snafu::ensure!(
        name.len() <= MAX_NAME,
        TarNameTooLongSnafu { name: filename }
    );
    h[..name.len()].copy_from_slice(name);

    let size = size as u64;
    snafu::ensure!(
        size <= MAX_SIZE,
        TarFileTooLargeSnafu {
            name: filename,
            size,
            max: MAX_SIZE,
        }
    );

    write_octal(&mut h[100..108], 0o644, 8);
    write_octal(&mut h[108..116], 0, 8);
    write_octal(&mut h[116..124], 0, 8);
    write_octal(&mut h[124..136], size, 12);
    write_octal(&mut h[136..148], 0, 12);

    // chksum: 8 spaces while computing.
    h[148..156].copy_from_slice(b"        ");
    h[156] = b'0'; // typeflag: regular file
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");

    let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
    let chk = format!("{sum:06o}\0 ");
    h[148..156].copy_from_slice(chk.as_bytes());

    Ok(h)
}

/// Write `value` as a ustar numeric field: `width - 1` zero-padded octal
/// digits followed by a NUL.
///
/// Callers must have checked that `value` fits; anything wider would be
/// silently truncated into a header that describes a different file.
fn write_octal(buf: &mut [u8], mut value: u64, width: usize) {
    let mut digits = vec![b'0'; width - 1];
    let mut i = digits.len();
    while value > 0 && i > 0 {
        i -= 1;
        digits[i] = b'0' + ((value & 0o7) as u8);
        value >>= 3;
    }
    debug_assert_eq!(value, 0, "numeric field overflowed {width} bytes");
    buf[..width - 1].copy_from_slice(&digits);
    buf[width - 1] = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_layout() {
        let tar = build_single_file_tar("hello.txt", b"hi\n").expect("build");
        // Header starts with the name.
        assert_eq!(&tar[..9], b"hello.txt");
        // Magic at offset 257.
        assert_eq!(&tar[257..263], b"ustar\0");
        // Content directly after the 512-byte header.
        assert_eq!(&tar[512..515], b"hi\n");
        // Total size: 512 (header) + 512 (content padded) + 1024 (eof) = 2048.
        assert_eq!(tar.len(), 2048);
    }

    #[test]
    fn checksum_is_octal_six_digits() {
        let tar = build_single_file_tar("a", b"x").expect("build");
        let chk = std::str::from_utf8(&tar[148..154]).unwrap();
        assert!(chk.chars().all(|c| c.is_ascii_digit() && c < '8'));
    }

    #[test]
    fn larger_content_padded_to_512() {
        let content = vec![b'a'; 600];
        let tar = build_single_file_tar("a", &content).expect("build");
        // 512 header + 1024 (600 → next 512 boundary) + 1024 eof = 2560
        assert_eq!(tar.len(), 2560);
        // First 600 bytes after header are content; next 424 are zeros.
        assert!(tar[512..1112].iter().all(|&b| b == b'a'));
        assert!(tar[1112..1536].iter().all(|&b| b == 0));
    }

    #[test]
    fn multi_file_archive() {
        let tar = build_archive(&[("a.txt", b"AAA"), ("b.txt", b"BBBB")]).expect("build");
        // 512 (a hdr) + 512 (a body padded) + 512 (b hdr) + 512 (b body padded)
        // + 1024 (eof) = 3072
        assert_eq!(tar.len(), 3072);
        assert_eq!(&tar[..5], b"a.txt");
        assert_eq!(&tar[512..515], b"AAA");
        // Second header begins at offset 1024.
        assert_eq!(&tar[1024..1029], b"b.txt");
        assert_eq!(&tar[1536..1540], b"BBBB");
    }

    #[test]
    fn an_overlong_name_is_an_error() {
        let name = "n".repeat(101);
        let err = build_single_file_tar(&name, b"x").expect_err("name too long");
        assert!(
            matches!(err, crate::Error::TarNameTooLong { .. }),
            "got {err:?}"
        );
        assert!(build_single_file_tar(&"n".repeat(100), b"x").is_ok());
    }

    /// The size field holds 11 octal digits. Truncating instead of erroring
    /// would produce a header describing a much smaller file, and a tar that
    /// silently loses the tail.
    #[test]
    fn a_size_that_does_not_fit_the_header_is_an_error() {
        let err = ustar_header("big", 1 << 33).expect_err("size too large");
        assert!(
            matches!(err, crate::Error::TarFileTooLarge { .. }),
            "got {err:?}"
        );
        assert!(ustar_header("big", (1 << 33) - 1).is_ok());
    }

    #[test]
    fn the_largest_representable_size_is_all_sevens() {
        let header = ustar_header("big", MAX_SIZE as usize).expect("header");
        assert_eq!(&header[124..136], b"77777777777\0");
    }
}
