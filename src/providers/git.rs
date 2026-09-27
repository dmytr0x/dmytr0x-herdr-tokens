use super::Error;
use std::collections::BTreeMap;

pub(super) fn parse(bytes: &[u8]) -> Result<BTreeMap<String, String>, Error> {
    let mut counts = [0u64; 4];
    if !bytes.is_empty() && bytes.last() != Some(&0) {
        return Err(Error::InvalidOutput);
    }
    let mut records = bytes
        .split_inclusive(|b| *b == 0)
        .map(|s| &s[..s.len() - 1]);
    while let Some(record) = records.next() {
        match record.first() {
            Some(b'1' | b'2' | b'u') => {
                let kind = record[0];
                let n = match kind {
                    b'1' => 9,
                    b'2' => 10,
                    _ => 11,
                };
                let fields: Vec<_> = record.splitn(n, |b| *b == b' ').collect();
                if fields.len() != n
                    || fields.iter().any(|s| s.is_empty())
                    || fields[1].len() != 2
                    || !fields[1].iter().all(|b| b".MADRCUT".contains(b))
                {
                    return Err(Error::InvalidOutput);
                }
                if fields[2].len() != 4
                    || !(fields[2] == b"N..."
                        || (fields[2][0] == b'S'
                            && fields[2][1..].iter().all(|b| b".CMU".contains(b))))
                {
                    return Err(Error::InvalidOutput);
                }
                let modes = if kind == b'u' { 4 } else { 3 };
                for mode in &fields[3..3 + modes] {
                    if mode.len() != 6 || !mode.iter().all(|b| (b'0'..=b'7').contains(b)) {
                        return Err(Error::InvalidOutput);
                    }
                }
                let hashes = if kind == b'u' { 3 } else { 2 };
                for hash in &fields[3 + modes..3 + modes + hashes] {
                    if ![40, 64].contains(&hash.len()) || !hash.iter().all(u8::is_ascii_hexdigit) {
                        return Err(Error::InvalidOutput);
                    }
                }
                if kind == b'2' {
                    let score = fields[8];
                    if score.len() < 2
                        || !b"RC".contains(&score[0])
                        || !score[1..].iter().all(u8::is_ascii_digit)
                        || std::str::from_utf8(&score[1..])
                            .ok()
                            .and_then(|s| s.parse::<u8>().ok())
                            .is_none_or(|n| n > 100)
                        || records.next().is_none_or(|s| s.is_empty())
                    {
                        return Err(Error::InvalidOutput);
                    }
                }
                if kind == b'u' {
                    counts[3] += 1;
                } else {
                    counts[0] += u64::from(fields[1][1] != b'.');
                    counts[1] += u64::from(fields[1][0] != b'.');
                }
            }
            Some(b'?') if record.len() > 2 && record[1] == b' ' => counts[2] += 1,
            _ => return Err(Error::InvalidOutput),
        }
    }
    Ok([
        "modified_files",
        "staged_files",
        "untracked_files",
        "conflict_files",
    ]
    .into_iter()
    .zip(counts)
    .map(|(s, n)| (s.into(), n.to_string()))
    .collect())
}

#[cfg(test)]
mod tests {
    use super::parse;
    #[test]
    fn byte_paths_renames_and_malformed_records() {
        let hash = "0".repeat(40);
        let mut bytes =
            format!("2 RM N... 100644 100644 100644 {hash} {hash} R100 new\nname\0").into_bytes();
        bytes.extend_from_slice(b"old\xffname\0? bad\xff\nname\0");
        let counts = parse(&bytes).unwrap();
        assert_eq!(counts["modified_files"], "1");
        assert_eq!(counts["staged_files"], "1");
        assert_eq!(counts["untracked_files"], "1");
        for invalid in [
            b"? name".as_slice(),
            b"! ignored\0",
            b"\0",
            b"1 garbage\0",
            b"? \0",
            b"3 unexpected\0",
        ] {
            assert!(parse(invalid).is_err());
        }
        assert!(
            parse(format!("2 R. N... 100644 100644 100644 {hash} {hash} R100 name\0").as_bytes())
                .is_err()
        );
        assert!(
            parse(
                format!("2 R. N... 100644 100644 100644 {hash} {hash} R101 name\0old\0").as_bytes()
            )
            .is_err()
        );
    }
}
