//! Shared decode pipeline used by both the folder watcher and manual drops.
//!
//! One file at a time, in queue order. Every step reports progress on the
//! `queue-progress` event and writes a line to the run log.

use serde_json::json;
use tauri::Emitter;

use crate::settings::Config;

/// Everything one job needs, decoupled from where the file came from.
pub struct Job {
    pub input: String,
    pub filename: String,
    /// Files queued behind this one, for the UI counter.
    pub pending: usize,
    /// Files already finished in this session.
    pub processed: usize,
}

/// Run the full pipeline for one NCM file.
///
/// Returns `Ok(output)` on success or the error message on failure; the caller
/// owns counters, notifications and the lifetime total.
pub fn process_job(app: &tauri::AppHandle, cfg: &Config, output: &std::path::Path, job: &Job) -> Result<std::path::PathBuf, String> {
    let input = std::path::Path::new(&job.input);
    let filename = job.filename.clone();
    let pending = job.pending;
    let done = job.processed;
    let stage = |status: &str, pct: u8| {
        let _ = app.emit("queue-progress", json!({
            "state": "processing", "file": filename, "status": status,
            "progress": pct, "queued": pending, "processed": done
        }));
    };

    let size = std::fs::metadata(input).map(|m| format!("{:.2} MB", m.len() as f64 / 1048576.0)).unwrap_or_else(|_| "大小未知".into());
    crate::logger::write(Some(app), "INFO", format!("开始处理: {} ({})", filename, size));

    let result = crate::ncm_decrypt::process_ncm_file_with_progress(input, output, |stage_name, pct| {
        let _ = app.emit("queue-progress", json!({
            "state": "processing", "file": filename, "status": stage_name,
            "progress": pct, "queued": pending, "processed": done
        }));
    });
    let out = match result {
        Ok(p) => p,
        Err(e) => return Err(e.to_string()),
    };
    crate::logger::write(Some(app), "INFO", format!("解密完成: {} → {}", filename, out.file_name().unwrap_or_default().to_string_lossy()));

    let stem = out.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let dir = out.parent().unwrap_or(std::path::Path::new("."));
    let cover_png = dir.join(format!("{stem}.cover.png"));
    let cover_jpg = dir.join(format!("{stem}.cover.jpg"));
    let is_flac = out.extension().and_then(|x| x.to_str()) == Some("flac");

    // The cover is always embedded: it is part of the decoded payload, not an
    // optional conversion step.
    stage("写入封面", 92);
    if let Ok(raw) = std::fs::read(&cover_png).or_else(|_| std::fs::read(&cover_jpg)) {
        let ok = if is_flac { crate::audio_tags::append_flac_cover(&out, &raw).is_ok() }
            else { crate::audio_tags::append_id3_cover(&out, &raw).is_ok() };
        crate::logger::write(Some(app), if ok {"INFO"} else {"ERROR"}, if ok { format!("写入封面: {} 字节", raw.len()) } else { "写入封面失败".to_string() });
    } else {
        crate::logger::write(Some(app), "INFO", "该文件不包含封面，跳过写入封面");
    }
    let _ = std::fs::remove_file(&cover_png);
    let _ = std::fs::remove_file(&cover_jpg);

    if cfg.embed_lrc {
        let lrc = input.with_extension("lrc");
        if let Ok(raw) = std::fs::read(&lrc) {
            stage("写入歌词", 95);
            let lyrics = crate::audio_tags::parse_lrc(&raw);
            let ok = if is_flac { crate::audio_tags::append_flac_lyrics(&out, &lyrics).is_ok() }
                else { crate::audio_tags::append_id3_lyrics(&out, &lyrics).is_ok() };
            crate::logger::write(Some(app), if ok {"INFO"} else {"ERROR"}, if ok { format!("写入歌词: {} 字节", lyrics.len()) } else { "写入歌词失败".to_string() });
        } else {
            crate::logger::write(Some(app), "INFO", "未发现同名 LRC 文件，跳过写入歌词");
        }
    }

    // Trimming runs last so it also removes tags the lyrics step added.
    if cfg.minimal_metadata {
        stage("整理元数据", 97);
        let ok = if is_flac { crate::audio_tags::minimal_flac(&out).is_ok() }
            else { crate::audio_tags::minimal_id3(&out).is_ok() };
        crate::logger::write(Some(app), if ok {"INFO"} else {"ERROR"}, if ok { "极简模式: 已精简元数据".to_string() } else { "极简模式处理失败".to_string() });
    }

    if cfg.shred_mode {
        stage("删除原始文件", 99);
        let ncm_ok = std::fs::remove_file(input).is_ok();
        let lrc_ok = std::fs::remove_file(input.with_extension("lrc")).is_ok();
        crate::logger::write(Some(app), if ncm_ok {"INFO"} else {"ERROR"}, format!("删除原始文件: NCM={} LRC={}", if ncm_ok {"成功"} else {"失败"}, if lrc_ok {"成功"} else {"无或失败"}));
    }

    Ok(out)
}
