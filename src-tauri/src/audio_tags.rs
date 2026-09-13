use std::{fs, io, path::Path};

/// `作词: X` -> `作词 : X`, matching the spacing used by reference players.
fn normalise_meta(s: &str) -> String {
    match s.split_once(':').or_else(|| s.split_once('：')) {
        Some((k, v)) => format!("{} : {}", k.trim(), v.trim()),
        None => s.to_string(),
    }
}

/// Convert a NetEase LRC file into the standard `[mm:ss.xx] text` form used by
/// media players.
///
/// NetEase mixes two line kinds:
///   * JSON meta lines  `{"t":0,"c":[{"tx":"作词: "},{"tx":"Abel Tesfaye"}]}`
///   * plain LRC lines  `[00:03.619] Oh woah, woah, oh, oh`
///
/// Both become timestamped text with two decimals, matching the reference
/// files (e.g. `[00:03.61] Oh woah, woah, oh, oh`). Bilingual files repeat a
/// timestamp for the translation, which is preserved as-is.
pub fn parse_lrc(data: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(data);
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim_start_matches('\u{feff}').trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(line) {
            let mut s = String::new();
            if let Some(parts) = obj.get("c").and_then(|x| x.as_array()) {
                for p in parts {
                    if let Some(t) = p.get("tx").and_then(|x| x.as_str()) {
                        s.push_str(t);
                    }
                }
            }
            if !s.is_empty() {
                // NetEase meta lines read "作词: name"; the reference format
                // spaces the colon on both sides.
                let s = normalise_meta(&s);
                let t = obj.get("t").and_then(|x| x.as_i64()).unwrap_or(0);
                out.push(format!("[{:02}:{:02}.{:02}] {}", t / 60000, (t % 60000) / 1000, (t % 1000) / 10, s));
            }
            continue;
        }
        // Plain LRC line: normalise the timestamp to two decimals.
        if let Some(rest) = line.strip_prefix('[') {
            if let Some(end) = rest.find(']') {
                let stamp = &rest[..end];
                let body = &rest[end + 1..];
                if let Some((mm, ss)) = stamp.split_once(':') {
                    let mm = mm.trim();
                    // Split seconds into whole part and fraction.
                    let (sec, frac) = match ss.split_once('.') {
                        Some((a, b)) => (a, Some(b)),
                        None => (ss, None),
                    };
                    if let (Ok(m), Ok(s)) = (mm.parse::<u32>(), sec.parse::<u32>()) {
                        let cs = match frac {
                            Some(f) => {
                                let digits: String = f.chars().filter(|c| c.is_ascii_digit()).collect();
                                let mut v: u32 = 0;
                                // Truncate to centiseconds; the reference files do not round.
                                for i in 0..2 { v = v * 10 + digits.chars().nth(i).and_then(|c| c.to_digit(10)).unwrap_or(0); }
                                v
                            }
                            None => 0,
                        };
                        out.push(format!("[{:02}:{:02}.{:02}]{}", m, s, cs.min(99), body));
                        continue;
                    }
                }
            }
        }
        out.push(line.to_string());
    }
    out.join("\n").into_bytes()
}

/// Insert a frame into the ID3v2 tag, replacing any frame with the same ID.
///
/// The decrypted NCM payload already carries an ID3 tag with TIT2/TPE1/TALB, so
/// a blind insert would leave two copies of every field and players would show
/// whichever one they happen to read first.
fn add_id3_frame(data: Vec<u8>, id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut frame = id.to_vec();
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&[0, 0]);
    frame.extend_from_slice(body);
    if data.starts_with(b"ID3") && data.len() >= 10 {
        let size = ((data[6] as usize & 0x7f) << 21) | ((data[7] as usize & 0x7f) << 14) | ((data[8] as usize & 0x7f) << 7) | (data[9] as usize & 0x7f);
        let tag_end = 10 + size;
        // Walk the frame list, dropping any frame that shares our ID.
        let mut kept: Vec<u8> = Vec::with_capacity(size + frame.len());
        let mut pos = 10usize;
        let mut insert_at: Option<usize> = None;
        while pos + 10 <= tag_end {
            let fid = &data[pos..pos + 4];
            if fid[0] == 0 { break; } // padding starts here
            let fsize = u32::from_be_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as usize;
            let next = pos + 10 + fsize;
            if next > tag_end { break; }
            if fid == id {
                // Remember where the first duplicate was so ordering is stable.
                if insert_at.is_none() { insert_at = Some(10 + kept.len()); }
            } else {
                kept.extend_from_slice(&data[pos..next]);
            }
            pos = next;
        }
        // No existing frame: append at the end of the frame list, before padding.
        let insert_at = insert_at.unwrap_or(10 + kept.len());
        let new_size = kept.len() + frame.len();
        let mut h = data[..10].to_vec();
        let head = insert_at - 10;
        h.extend_from_slice(&kept[..head]);
        h.extend_from_slice(&frame);
        h.extend_from_slice(&kept[head..]);
        h[6] = ((new_size >> 21) & 0x7f) as u8;
        h[7] = ((new_size >> 14) & 0x7f) as u8;
        h[8] = ((new_size >> 7) & 0x7f) as u8;
        h[9] = (new_size & 0x7f) as u8;
        h.extend_from_slice(&data[tag_end..]);
        h
    } else {
        let mut t = b"ID3\x03\x00\x00\x00\x00\x00\x00".to_vec();
        t.extend(frame);
        t.extend(data);
        t
    }
}

/// Rewrite an ID3v2 tag so it keeps only Title, Artist, cover and lyrics.
///
/// All other frames (album, track number, comments, encoder, ...) are dropped,
/// which is what the "极简模式" option promises. Lyrics survive because the
/// option is independent from the LRC embedding feature.
pub fn minimal_id3(path: &Path) -> io::Result<()> {
    let data = fs::read(path)?;
    if !data.starts_with(b"ID3") || data.len() < 10 {
        return Ok(()); // no tag: nothing to trim
    }
    let size = ((data[6] as usize & 0x7f) << 21) | ((data[7] as usize & 0x7f) << 14)
        | ((data[8] as usize & 0x7f) << 7) | (data[9] as usize & 0x7f);
    let tag_end = 10 + size;
    if tag_end > data.len() {
        return Ok(());
    }

    // Collect the frames we want to keep, in order.
    // Lyrics live in TXXX so 极简模式 must keep it too.
    let keep = [&b"TIT2"[..], &b"TPE1"[..], &b"APIC"[..], &b"USLT"[..], &b"SYLT"[..], &b"TXXX"[..]];
    let mut frames: Vec<Vec<u8>> = Vec::new();
    let mut pos = 10usize;
    while pos + 10 <= tag_end {
        if data[pos] == 0 { break; } // padding
        let fsize = u32::from_be_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let next = pos + 10 + fsize;
        if next > tag_end { break; }
        if keep.contains(&&data[pos..pos + 4]) {
            frames.push(data[pos..next].to_vec());
        }
        pos = next;
    }

    let mut new_tag = Vec::new();
    for f in &frames { new_tag.extend_from_slice(f); }
    let new_size = new_tag.len();
    let mut out = b"ID3\x03\x00\x00".to_vec();
    out.push(((new_size >> 21) & 0x7f) as u8);
    out.push(((new_size >> 14) & 0x7f) as u8);
    out.push(((new_size >> 7) & 0x7f) as u8);
    out.push((new_size & 0x7f) as u8);
    out.extend_from_slice(&new_tag);
    out.extend_from_slice(&data[tag_end..]);
    fs::write(path, out)
}

/// Rewrite a FLAC VORBIS_COMMENT block so it keeps only TITLE, ARTIST and LYRICS.
///
/// The PICTURE block is left untouched, matching "只保留 Title Artist 和封面".
/// LYRICS is preserved because 极简模式 and lyric embedding are independent.
pub fn minimal_flac(path: &Path) -> io::Result<()> {
    let mut data = fs::read(path)?;
    if !data.starts_with(b"fLaC") {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not FLAC"));
    }
    let mut pos = 4usize;
    let mut found: Option<(usize, usize, usize, bool)> = None; // header, body, len, is_last
    loop {
        if pos + 4 > data.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated FLAC metadata"));
        }
        let is_last = data[pos] & 0x80 != 0;
        let ty = data[pos] & 0x7f;
        let len = ((data[pos + 1] as usize) << 16) | ((data[pos + 2] as usize) << 8) | (data[pos + 3] as usize);
        if ty == 4 {
            found = Some((pos, pos + 4, len, is_last));
            break;
        }
        pos += 4 + len;
        if pos > data.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated FLAC metadata block"));
        }
        if is_last { break; }
    }
    let Some((header, body_start, len, is_last)) = found else { return Ok(()) };

    let body = &data[body_start..body_start + len];
    let vendor_len = u32::from_le_bytes(body[0..4].try_into().unwrap()) as usize;
    let vendor = body[4..4 + vendor_len].to_vec();
    let mut p = 4 + vendor_len;
    let count = u32::from_le_bytes(body[p..p + 4].try_into().unwrap()) as usize;
    p += 4;

    let mut kept: Vec<Vec<u8>> = Vec::new();
    for _ in 0..count {
        if p + 4 > body.len() { break; }
        let n = u32::from_le_bytes(body[p..p + 4].try_into().unwrap()) as usize;
        p += 4;
        if p + n > body.len() { break; }
        let comment = body[p..p + n].to_vec();
        p += n;
        let text = String::from_utf8_lossy(&comment);
        let key = text.split('=').next().unwrap_or("").to_ascii_uppercase();
        if key == "TITLE" || key == "ARTIST" || key == "LYRICS" { kept.push(comment); }
    }

    let mut new_body = Vec::new();
    new_body.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    new_body.extend_from_slice(&vendor);
    new_body.extend_from_slice(&(kept.len() as u32).to_le_bytes());
    for c in &kept {
        new_body.extend_from_slice(&(c.len() as u32).to_le_bytes());
        new_body.extend_from_slice(c);
    }

    let mut block = vec![4u8 | if is_last { 0x80 } else { 0 }];
    block.extend_from_slice(&(new_body.len() as u32).to_be_bytes()[1..]);
    block.extend_from_slice(&new_body);
    data.splice(header..body_start + len, block);
    fs::write(path, data)
}

/// Append lyrics as a `TXXX` frame named `LYRICS`, UTF-16 encoded with CRLF line
/// endings. This matches the layout used by the reference MP3 file and is what
/// most Chinese players (NetEase, Foobar2000) read back.
pub fn append_id3_lyrics(path: &Path, lyrics: &[u8]) -> io::Result<()> {
    let text = String::from_utf8_lossy(lyrics).replace('\n', "\r\n");
    // TXXX body: encoding(1) + description + NUL + value
    let mut b = vec![1u8]; // UTF-16 with BOM
    b.extend_from_slice(&[0xff, 0xfe]); // BOM
    for u in "LYRICS".encode_utf16() { b.extend_from_slice(&u.to_le_bytes()); }
    b.extend_from_slice(&[0, 0]); // description terminator (UTF-16 NUL)
    b.extend_from_slice(&[0xff, 0xfe]); // BOM for the value
    for u in text.encode_utf16() { b.extend_from_slice(&u.to_le_bytes()); }
    fs::write(path, add_id3_frame(fs::read(path)?, b"TXXX", &b))
}

pub fn append_id3_metadata(path: &Path, title: &str, artist: &str, album: &str, pic_type: u8) -> io::Result<()> {
    let mut d = fs::read(path)?;
    for (id, value) in [(&b"TIT2" as &[u8; 4], title), (&b"TPE1" as &[u8; 4], artist), (&b"TALB" as &[u8; 4], album)] {
        let mut b = vec![3];
        b.extend_from_slice(value.as_bytes());
        d = add_id3_frame(d, id, &b);
    }
    let _ = pic_type;
    fs::write(path, d)
}

/// Insert one metadata block into a FLAC stream.
///
/// FLAC requires the last metadata block to carry the `is-last` flag. The new
/// block is inserted at the end of the chain (after the existing last block)
/// and becomes the new last block; the previous last block loses its flag.
fn insert_flac_block(data: &mut Vec<u8>, block_type: u8, payload: &[u8]) -> io::Result<()> {
    if !data.starts_with(b"fLaC") {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not FLAC"));
    }
    let mut pos = 4usize;
    let last_header;
    loop {
        if pos + 4 > data.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated FLAC metadata"));
        }
        let is_last = data[pos] & 0x80 != 0;
        let len = ((data[pos + 1] as usize) << 16) | ((data[pos + 2] as usize) << 8) | (data[pos + 3] as usize);
        let header = pos;
        pos += 4 + len;
        if pos > data.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated FLAC metadata block"));
        }
        if is_last {
            last_header = header;
            break;
        }
    }
    // Clear the is-last flag on the previous final block and append ours.
    data[last_header] &= 0x7f;
    let mut block = vec![block_type | 0x80];
    block.extend_from_slice(&(payload.len() as u32).to_be_bytes()[1..]);
    block.extend_from_slice(payload);
    data.splice(pos..pos, block);
    Ok(())
}

/// Merge comments into the existing VORBIS_COMMENT block, creating one if the
/// stream has none. FLAC permits only a single VORBIS_COMMENT block, so adding
/// a second block would make the file invalid.
fn upsert_flac_comments(data: &mut Vec<u8>, entries: &[String]) -> io::Result<()> {
    if !data.starts_with(b"fLaC") {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not FLAC"));
    }
    let mut pos = 4usize;
    let mut found: Option<(usize, usize, bool)> = None; // (header, body_start, is_last)
    loop {
        if pos + 4 > data.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated FLAC metadata"));
        }
        let is_last = data[pos] & 0x80 != 0;
        let ty = data[pos] & 0x7f;
        let len = ((data[pos + 1] as usize) << 16) | ((data[pos + 2] as usize) << 8) | (data[pos + 3] as usize);
        if ty == 4 {
            found = Some((pos, pos + 4, is_last));
            break;
        }
        pos += 4 + len;
        if pos > data.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated FLAC metadata block"));
        }
        if is_last {
            break;
        }
    }

    let keys: Vec<&str> = entries.iter().filter_map(|e| e.split_once('=').map(|x| x.0)).collect();

    if let Some((header, body_start, _)) = found {
        let len = ((data[header + 1] as usize) << 16) | ((data[header + 2] as usize) << 8) | (data[header + 3] as usize);
        let body = &data[body_start..body_start + len];
        // Read existing vendor + comments.
        let vendor_len = u32::from_le_bytes(body[0..4].try_into().unwrap()) as usize;
        let vendor = body[4..4 + vendor_len].to_vec();
        let mut p = 4 + vendor_len;
        let count = u32::from_le_bytes(body[p..p + 4].try_into().unwrap()) as usize;
        p += 4;
        let mut kept: Vec<Vec<u8>> = Vec::new();
        for _ in 0..count {
            let n = u32::from_le_bytes(body[p..p + 4].try_into().unwrap()) as usize;
            p += 4;
            let comment = body[p..p + n].to_vec();
            p += n;
            let text = String::from_utf8_lossy(&comment);
            let key = text.split('=').next().unwrap_or("").to_string();
            if !keys.iter().any(|k| k.eq_ignore_ascii_case(&key)) {
                kept.push(comment);
            }
        }
        for e in entries {
            kept.push(e.as_bytes().to_vec());
        }
        // Rebuild the block.
        let mut new_body = Vec::new();
        new_body.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        new_body.extend_from_slice(&vendor);
        new_body.extend_from_slice(&(kept.len() as u32).to_le_bytes());
        for c in &kept {
            new_body.extend_from_slice(&(c.len() as u32).to_le_bytes());
            new_body.extend_from_slice(c);
        }
        let mut block = vec![data[header] & 0x80 | 4];
        block.extend_from_slice(&(new_body.len() as u32).to_be_bytes()[1..]);
        block.extend_from_slice(&new_body);
        data.splice(header..body_start + len, block);
        Ok(())
    } else {
        let mut payload = Vec::new();
        payload.extend_from_slice(&0u32.to_le_bytes()); // vendor length
        payload.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for e in entries {
            payload.extend_from_slice(&(e.len() as u32).to_le_bytes());
            payload.extend_from_slice(e.as_bytes());
        }
        insert_flac_block(data, 4, &payload)
    }
}

pub fn append_flac_lyrics(path: &Path, lyrics: &[u8]) -> io::Result<()> {
    if lyrics.is_empty() {
        return Ok(());
    }
    let mut data = fs::read(path)?;
    let text = String::from_utf8_lossy(lyrics);
    upsert_flac_comments(&mut data, &[format!("LYRICS={text}")])?;
    fs::write(path, data)
}

pub fn append_flac_metadata(path: &Path, title: &str, artist: &str, album: &str) -> io::Result<()> {
    let mut data = fs::read(path)?;
    let entries = [format!("TITLE={title}"), format!("ARTIST={artist}"), format!("ALBUM={album}")];
    upsert_flac_comments(&mut data, &entries)?;
    fs::write(path, data)
}

pub fn append_id3_cover(path: &Path, cover: &[u8]) -> io::Result<()> {
    let mime: &[u8] = if cover.starts_with(&[0x89, 0x50, 0x4e, 0x47]) { b"image/png" } else { b"image/jpeg" };
    // APIC body: text encoding, MIME type (NUL terminated), picture type,
    // description (NUL terminated with its encoding), then the image data.
    let mut b = vec![0u8]; // ISO-8859-1 for the MIME/description text
    b.extend_from_slice(mime);
    b.push(0); // MIME terminator
    b.push(3); // picture type 3 = front cover
    b.push(0); // empty description
    b.extend_from_slice(cover);
    fs::write(path, add_id3_frame(fs::read(path)?, b"APIC", &b))
}

pub fn append_flac_cover(path: &Path, cover: &[u8]) -> io::Result<()> {
    let mut data = fs::read(path)?;
    let mime = if cover.starts_with(&[0x89, 0x50, 0x4e, 0x47]) { "image/png" } else { "image/jpeg" };
    let mut p = Vec::new();
    p.extend_from_slice(&3u32.to_be_bytes()); // picture type: front cover
    p.extend_from_slice(&(mime.len() as u32).to_be_bytes());
    p.extend_from_slice(mime.as_bytes());
    p.extend_from_slice(&0u32.to_be_bytes()); // description length
    p.extend_from_slice(&0u32.to_be_bytes()); // width
    p.extend_from_slice(&0u32.to_be_bytes()); // height
    p.extend_from_slice(&0u32.to_be_bytes()); // depth
    p.extend_from_slice(&0u32.to_be_bytes()); // colors
    p.extend_from_slice(&(cover.len() as u32).to_be_bytes());
    p.extend_from_slice(cover);
    insert_flac_block(&mut data, 6, &p)?;
    fs::write(path, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collect the frame IDs present in an ID3v2 tag, in order.
    fn frame_ids(data: &[u8]) -> Vec<String> {
        let mut ids = Vec::new();
        if !data.starts_with(b"ID3") || data.len() < 10 { return ids; }
        let size = ((data[6] as usize & 0x7f) << 21) | ((data[7] as usize & 0x7f) << 14) | ((data[8] as usize & 0x7f) << 7) | (data[9] as usize & 0x7f);
        let tag_end = 10 + size;
        let mut pos = 10usize;
        while pos + 10 <= tag_end {
            if data[pos] == 0 { break; }
            let fsize = u32::from_be_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as usize;
            let next = pos + 10 + fsize;
            if next > tag_end { break; }
            ids.push(String::from_utf8_lossy(&data[pos..pos + 4]).into_owned());
            pos = next;
        }
        ids
    }

    /// Writing metadata twice must not duplicate frames in the ID3 tag.
    #[test]
    fn id3_frames_are_replaced_not_duplicated() {
        let dir = std::env::temp_dir().join("nadt-tag-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dup.mp3");
        // Minimal payload: an ID3 tag with one TIT2 frame, then audio bytes.
        let mut data = b"ID3\x03\x00\x00\x00\x00\x00\x0e".to_vec();
        let body = b"\x03Old";
        data.extend_from_slice(b"TIT2");
        data.extend_from_slice(&(body.len() as u32).to_be_bytes());
        data.extend_from_slice(&[0, 0]);
        data.extend_from_slice(body);
        data.extend_from_slice(b"\xff\xfb\x90\x00audio");
        std::fs::write(&path, &data).unwrap();

        append_id3_metadata(&path, "New Title", "New Artist", "New Album", 3).unwrap();
        append_id3_metadata(&path, "New Title", "New Artist", "New Album", 3).unwrap();

        let out = std::fs::read(&path).unwrap();
        let ids = frame_ids(&out);
        for id in ["TIT2", "TPE1", "TALB"] {
            assert_eq!(ids.iter().filter(|x| x.as_str() == id).count(), 1, "{id} duplicated in {ids:?}");
        }
        // The replacement value must be the one that survives.
        assert!(out.windows(9).any(|w| w == b"New Title"));
        assert!(!out.windows(3).any(|w| w == b"Old"));
    }
}
