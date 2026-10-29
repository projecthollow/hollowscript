//! The Language Server Protocol's message framing: each message is
//! `Content-Length: N\r\n\r\n` followed by exactly `N` bytes of UTF-8 JSON.
//! Framing is independent of what the JSON says, so it is written and tested
//! here on its own, ahead of the JSON-RPC method dispatch that reads the body.

use std::io::{BufRead, Read, Write};

// A source file is limited to 4 MiB; this leaves room for JSON escaping and
// protocol fields without allowing a peer to allocate an arbitrary body.
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 8 * 1024;

/// Reads one framed message from `input`.
///
/// # Errors
///
/// Returns an error if the header is malformed, `Content-Length` is missing,
/// or the stream ends before the declared length is read. Returns `Ok(None)`
/// at a clean end of stream before any header line is read (the normal way a
/// client closes the connection).
pub(crate) fn read_message(input: &mut impl BufRead) -> std::io::Result<Option<Vec<u8>>> {
    let mut content_length: Option<usize> = None;
    let mut saw_any_header = false;
    let mut header_bytes = 0;
    loop {
        let mut line = String::new();
        let remaining = MAX_HEADER_BYTES - header_bytes;
        let bytes_read = (&mut *input)
            .take((remaining + 1) as u64)
            .read_line(&mut line)?;
        header_bytes += bytes_read;
        if header_bytes > MAX_HEADER_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "header exceeds 8 KiB",
            ));
        }
        if bytes_read == 0 {
            return if saw_any_header {
                Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "stream ended inside a header",
                ))
            } else {
                Ok(None)
            };
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break; // the blank line ends the header block
        }
        saw_any_header = true;
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().ok();
            }
        }
    }
    let Some(length) = content_length else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "no Content-Length header",
        ));
    };
    if length > MAX_BODY_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "message body exceeds 32 MiB",
        ));
    }
    let mut body = vec![0u8; length];
    input.read_exact(&mut body)?;
    Ok(Some(body))
}

/// Writes one framed message to `output`.
///
/// # Errors
///
/// Returns whatever writing to `output` returns.
pub(crate) fn write_message(output: &mut impl Write, body: &[u8]) -> std::io::Result<()> {
    write!(output, "Content-Length: {}\r\n\r\n", body.len())?;
    output.write_all(body)?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::{read_message, write_message};
    use std::io::Cursor;

    #[test]
    fn an_untrusted_body_length_is_bounded_before_allocation() {
        for length in [usize::MAX, 32 * 1024 * 1024 + 1] {
            let raw = format!("Content-Length: {length}\r\n\r\n");
            let error = read_message(&mut Cursor::new(raw)).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn an_unterminated_header_stops_at_the_header_budget() {
        let mut input = Cursor::new(vec![b'x'; 32 * 1024]);
        let error = read_message(&mut input).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(input.position() <= 8193);
    }

    #[test]
    fn many_short_headers_share_one_budget() {
        let mut input = Cursor::new("X: 1\r\n".repeat(4096));
        let error = read_message(&mut input).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(input.position() <= 8193);
    }

    #[test]
    fn a_written_message_reads_back_exactly() {
        let mut buf = Vec::new();
        write_message(&mut buf, br#"{"jsonrpc":"2.0"}"#).unwrap();
        assert_eq!(buf, b"Content-Length: 17\r\n\r\n{\"jsonrpc\":\"2.0\"}");
        let mut cursor = Cursor::new(buf);
        let mut reader = std::io::BufReader::new(&mut cursor);
        let read = read_message(&mut reader).unwrap().unwrap();
        assert_eq!(read, br#"{"jsonrpc":"2.0"}"#);
    }

    #[test]
    fn header_names_are_matched_case_insensitively_and_extra_headers_are_ignored() {
        let raw = b"content-TYPE: application/json\r\nContent-Length: 2\r\n\r\n{}";
        let mut cursor = Cursor::new(raw.to_vec());
        let mut reader = std::io::BufReader::new(&mut cursor);
        assert_eq!(read_message(&mut reader).unwrap(), Some(b"{}".to_vec()));
    }

    #[test]
    fn a_clean_close_before_any_header_is_a_normal_end() {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let mut reader = std::io::BufReader::new(&mut cursor);
        assert_eq!(read_message(&mut reader).unwrap(), None);
    }

    #[test]
    fn a_missing_content_length_is_an_error_not_a_panic() {
        let raw = b"X-Other: 1\r\n\r\nbody";
        let mut cursor = Cursor::new(raw.to_vec());
        let mut reader = std::io::BufReader::new(&mut cursor);
        assert!(read_message(&mut reader).is_err());
    }

    #[test]
    fn a_truncated_body_is_an_error_not_a_panic() {
        let raw = b"Content-Length: 100\r\n\r\nshort";
        let mut cursor = Cursor::new(raw.to_vec());
        let mut reader = std::io::BufReader::new(&mut cursor);
        assert!(read_message(&mut reader).is_err());
    }

    #[test]
    fn two_messages_in_one_stream_are_read_in_order() {
        let mut buf = Vec::new();
        write_message(&mut buf, b"one").unwrap();
        write_message(&mut buf, b"two").unwrap();
        let mut cursor = Cursor::new(buf);
        let mut reader = std::io::BufReader::new(&mut cursor);
        assert_eq!(read_message(&mut reader).unwrap(), Some(b"one".to_vec()));
        assert_eq!(read_message(&mut reader).unwrap(), Some(b"two".to_vec()));
        assert_eq!(read_message(&mut reader).unwrap(), None);
    }
}
