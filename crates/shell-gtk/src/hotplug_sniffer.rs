//! The content sniffer GNOME Shell 51 falls back to when a new mount has
//! no `x-content/*` type of its own (`autorunManager.js`
//! `ContentTypeDiscoverer.guessContentTypes` calling
//! `org.gnome.Shell.HotplugSniffer.SniffURI`). Ported in-process from
//! `src/hotplug-sniffer/shell-mime-sniffer.c` (GPL-2.0-or-later) and its
//! MIME tables in `hotplug-mimetypes.h`: a breadth-first count of the
//! mount's files by content type, cut off by a 1.5 s watchdog, scored as
//! video, audio, pictures or documents.

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use gio::prelude::*;
use gtk4::{gdk_pixbuf, gio, glib};

/// `WATCHDOG_TIMEOUT` (ms): counting stops and what was seen is scored.
const WATCHDOG: Duration = Duration::from_millis(1500);
/// `HIGH_SCORE_RATIO`: a second or third type is reported at this share.
const HIGH_SCORE_RATIO: f64 = 0.10;
/// `DIRECTORY_LOAD_ITEMS_PER_CALLBACK`.
const ITEMS_PER_BATCH: i32 = 100;
/// `LOADER_ATTRS`.
const LOADER_ATTRS: &str = "standard::type,standard::name,standard::content-type";

/// Files counted per category (`DeepCountState`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub image: u32,
    pub video: u32,
    pub docs: u32,
    pub audio: u32,
}

impl Counts {
    /// `add_content_type_to_cache`: image, then video, documents, audio.
    pub fn add(&mut self, content_type: &str, images: &HashSet<String>) {
        if images.contains(content_type) {
            self.image += 1;
        } else if VIDEO_MIMETYPES.contains(&content_type) {
            self.video += 1;
        } else if DOCS_MIMETYPES.contains(&content_type) {
            self.docs += 1;
        } else if AUDIO_MIMETYPES.contains(&content_type) {
            self.audio += 1;
        }
    }

    fn total(&self) -> u32 {
        self.image + self.video + self.docs + self.audio
    }
}

/// `prepare_async_result`: nothing when no file matched; otherwise the
/// best-scoring type, then the second and third while each holds at least
/// `HIGH_SCORE_RATIO` of the matched files.
pub fn classify(counts: &Counts) -> Vec<&'static str> {
    let total = counts.total();
    if total == 0 {
        return Vec::new();
    }
    let ratio = |n: u32| f64::from(n) / f64::from(total);
    let mut results = [
        ("x-content/video", ratio(counts.video)),
        ("x-content/audio", ratio(counts.audio)),
        ("x-content/pictures", ratio(counts.image)),
        ("x-content/documents", ratio(counts.docs)),
    ];
    results.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut sniffed = vec![results[0].0];
    for (kind, ratio) in &results[1..3] {
        if *ratio < HIGH_SCORE_RATIO {
            break;
        }
        sniffed.push(kind);
    }
    sniffed
}

/// The image types gdk-pixbuf can load (`init_mimetypes`).
pub fn image_types() -> HashSet<String> {
    gdk_pixbuf::Pixbuf::formats()
        .iter()
        .flat_map(|format| format.mime_types())
        .map(String::from)
        .collect()
}

/// Count `root` breadth-first (`deep_count_load` and friends) until done
/// or `cancellable` fires; blocking.
fn deep_count(
    root: &gio::File,
    images: &HashSet<String>,
    cancellable: &gio::Cancellable,
) -> Counts {
    let mut counts = Counts::default();
    let mut pending = VecDeque::from([root.clone()]);
    while let Some(dir) = pending.pop_front() {
        if cancellable.is_cancelled() {
            break;
        }
        let Ok(children) = dir.enumerate_children(
            LOADER_ATTRS,
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
            Some(cancellable),
        ) else {
            continue;
        };
        loop {
            let Ok(batch) = next_batch(&children, cancellable) else {
                break;
            };
            if batch.is_empty() {
                break;
            }
            for info in batch {
                if info.file_type() == gio::FileType::Directory {
                    pending.push_back(dir.child(info.name()));
                } else if let Some(content_type) = info.content_type() {
                    counts.add(&content_type, images);
                }
            }
        }
        let _ = children.close(gio::Cancellable::NONE);
    }
    counts
}

fn next_batch(
    children: &gio::FileEnumerator,
    cancellable: &gio::Cancellable,
) -> Result<Vec<gio::FileInfo>, glib::Error> {
    let mut batch = Vec::new();
    for _ in 0..ITEMS_PER_BATCH {
        match children.next_file(Some(cancellable))? {
            Some(info) => batch.push(info),
            None => break,
        }
    }
    Ok(batch)
}

/// `SniffURI(uri)`: the content types sniffed under a directory URI,
/// empty when it is not a readable directory. Runs off the main thread.
pub async fn sniff_uri(uri: String) -> Vec<String> {
    let images = image_types();
    let cancellable = gio::Cancellable::new();
    let watchdog = cancellable.clone();
    // A watchdog firing after the count finished cancels nothing.
    glib::timeout_add_once(WATCHDOG, move || watchdog.cancel());
    gio::spawn_blocking(move || {
        let root = gio::File::for_uri(&uri);
        // `query_info_async_ready_cb`: only a directory is counted.
        let is_dir = root.query_file_type(gio::FileQueryInfoFlags::NONE, Some(&cancellable))
            == gio::FileType::Directory;
        if !is_dir {
            return Vec::new();
        }
        let counts = deep_count(&root, &images, &cancellable);
        classify(&counts).into_iter().map(String::from).collect()
    })
    .await
    .unwrap_or_default()
}

/// `docs_mimetypes` in `hotplug-mimetypes.h`.
const DOCS_MIMETYPES: &[&str] = &[
    "application/vnd.oasis.opendocument.text",
    "application/vnd.oasis.opendocument.presentation",
    "application/vnd.oasis.opendocument.spreadsheet",
    "application/msword",
    "application/vnd.ms-excel",
    "application/vnd.ms-powerpoint",
    "application/rtf",
    "application/pdf",
    "application/x-bzpdf",
    "application/x-gzpdf",
    "application/x-xzpdf",
    "application/postscript",
    "application/x-bzpostscript",
    "application/x-gzpostscript",
    "image/x-eps",
    "image/x-bzeps",
    "image/x-gzeps",
    "application/x-dvi",
    "application/x-bzdvi",
    "application/x-gzdvi",
    "image/vnd.djvu",
    "application/x-cbr",
    "application/x-cbz",
    "application/x-cb7",
    "application/x-cbt",
];

/// `video_mimetypes` in `hotplug-mimetypes.h`.
const VIDEO_MIMETYPES: &[&str] = &[
    "application/mxf",
    "application/ogg",
    "application/ram",
    "application/sdp",
    "application/vnd.ms-wpl",
    "application/vnd.rn-realmedia",
    "application/x-extension-m4a",
    "application/x-extension-mp4",
    "application/x-flash-video",
    "application/x-matroska",
    "application/x-netshow-channel",
    "application/x-ogg",
    "application/x-quicktimeplayer",
    "application/x-shorten",
    "image/vnd.rn-realpix",
    "image/x-pict",
    "misc/ultravox",
    "text/x-google-video-pointer",
    "video/3gpp",
    "video/dv",
    "video/fli",
    "video/flv",
    "video/mp2t",
    "video/mp4",
    "video/mp4v-es",
    "video/mpeg",
    "video/msvideo",
    "video/ogg",
    "video/quicktime",
    "video/vivo",
    "video/vnd.divx",
    "video/vnd.rn-realvideo",
    "video/vnd.vivo",
    "video/webm",
    "video/x-anim",
    "video/x-avi",
    "video/x-flc",
    "video/x-fli",
    "video/x-flic",
    "video/x-flv",
    "video/x-m4v",
    "video/x-matroska",
    "video/x-mpeg",
    "video/x-ms-asf",
    "video/x-ms-asx",
    "video/x-msvideo",
    "video/x-ms-wm",
    "video/x-ms-wmv",
    "video/x-ms-wmx",
    "video/x-ms-wvx",
    "video/x-nsv",
    "video/x-ogm+ogg",
    "video/x-theora+ogg",
    "video/x-totem-stream",
];

/// `audio_mimetypes` in `hotplug-mimetypes.h`.
const AUDIO_MIMETYPES: &[&str] = &[
    "audio/3gpp",
    "audio/ac3",
    "audio/AMR",
    "audio/AMR-WB",
    "audio/basic",
    "audio/flac",
    "audio/midi",
    "audio/mp2",
    "audio/mp4",
    "audio/mpeg",
    "audio/ogg",
    "audio/prs.sid",
    "audio/vnd.rn-realaudio",
    "audio/x-aiff",
    "audio/x-ape",
    "audio/x-flac",
    "audio/x-gsm",
    "audio/x-it",
    "audio/x-m4a",
    "audio/x-matroska",
    "audio/x-mod",
    "audio/x-mp3",
    "audio/x-mpeg",
    "audio/x-ms-asf",
    "audio/x-ms-asx",
    "audio/x-ms-wax",
    "audio/x-ms-wma",
    "audio/x-musepack",
    "audio/x-pn-aiff",
    "audio/x-pn-au",
    "audio/x-pn-wav",
    "audio/x-pn-windows-acm",
    "audio/x-realaudio",
    "audio/x-real-audio",
    "audio/x-sbc",
    "audio/x-speex",
    "audio/x-tta",
    "audio/x-wav",
    "audio/x-wavpack",
    "audio/x-vorbis",
    "audio/x-vorbis+ogg",
    "audio/x-xm",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn images() -> HashSet<String> {
        ["image/jpeg".to_owned(), "image/png".to_owned()].into()
    }

    #[test]
    fn sniffer_scores_the_dominant_type_then_high_scorers() {
        let mut counts = Counts::default();
        for t in ["image/jpeg"; 8] {
            counts.add(t, &images());
        }
        counts.add("audio/mpeg", &images());
        counts.add("text/plain", &images());
        // 8 pictures, 1 audio (1/9 >= 0.10), unmatched text ignored.
        assert_eq!(counts.image, 8);
        assert_eq!(classify(&counts), ["x-content/pictures", "x-content/audio"]);

        let mut low = Counts {
            image: 20,
            audio: 1,
            ..Counts::default()
        };
        assert_eq!(classify(&low), ["x-content/pictures"]);
        low.video = 10;
        assert_eq!(classify(&low), ["x-content/pictures", "x-content/video"]);
        assert!(classify(&Counts::default()).is_empty());
    }

    #[test]
    fn sniffer_prefers_images_over_other_tables() {
        // image/x-pict is in GNOME's video table, but a pixbuf image
        // type wins first, as in `add_content_type_to_cache`.
        let mut images = images();
        images.insert("image/x-pict".into());
        let mut counts = Counts::default();
        counts.add("image/x-pict", &images);
        counts.add("application/pdf", &images);
        counts.add("video/mp4", &images);
        counts.add("audio/flac", &images);
        assert_eq!(
            counts,
            Counts {
                image: 1,
                video: 1,
                docs: 1,
                audio: 1
            }
        );
    }

    #[test]
    fn sniffer_counts_a_real_directory_tree() {
        let dir = std::env::temp_dir().join(format!("tuna-sniff-{}", std::process::id()));
        let nested = dir.join("DCIM/100");
        std::fs::create_dir_all(&nested).unwrap();
        let png = [
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, b'I', b'H', b'D', b'R',
        ];
        for i in 0..4 {
            std::fs::write(nested.join(format!("p{i}.png")), png).unwrap();
        }
        std::fs::write(dir.join("notes.txt"), b"plain").unwrap();
        let cancellable = gio::Cancellable::new();
        let counts = deep_count(&gio::File::for_path(&dir), &images(), &cancellable);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(counts.image, 4, "{counts:?}");
        assert_eq!(classify(&counts), ["x-content/pictures"]);
        // A cancelled count (the watchdog fired) scores nothing more.
        cancellable.cancel();
        assert_eq!(
            deep_count(&gio::File::for_path("/"), &images(), &cancellable),
            Counts::default()
        );
    }
}
