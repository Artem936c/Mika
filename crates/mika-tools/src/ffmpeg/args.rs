//! Argument lists for FFmpeg, built without running anything.
//!
//! Every input is read through the `file` protocol only, every output is
//! written with `-n` (never over a file), and no text of the model goes in
//! as it is: times and numbers are parsed and formatted here.

use std::path::Path;

use super::time::arg_time;
use super::wav::Loudness;

pub(crate) fn s(items: &[&str]) -> Vec<String> {
    items.iter().map(|x| x.to_string()).collect()
}

/// Video containers a result may keep.
pub(crate) const VIDEO_EXTS: &[&str] = &["mp4", "mkv", "mov", "webm"];
/// Sound formats with an encoder of their own.
pub(crate) const AUDIO_EXTS: &[&str] = &["mp3", "m4a", "wav", "flac", "ogg", "opus"];

/// A video result keeps the input's container when it is a common one.
pub(crate) fn video_ext(input_ext: &str) -> &'static str {
    VIDEO_EXTS
        .iter()
        .find(|e| **e == input_ext)
        .copied()
        .unwrap_or("mp4")
}

/// A sound result keeps the input's format when it can be written.
pub(crate) fn audio_ext(input_ext: &str) -> &'static str {
    AUDIO_EXTS
        .iter()
        .find(|e| **e == input_ext)
        .copied()
        .unwrap_or("m4a")
}

/// May a stream with `codec` go into `ext` as it is?
pub(crate) fn fits(ext: &str, codec: &str, video: bool) -> bool {
    match (ext, video) {
        ("mkv", _) => true,
        ("mp4", true) => matches!(codec, "h264" | "hevc" | "av1" | "mpeg4"),
        // FFmpeg writes AV1 into MP4 only.
        ("mov", true) => matches!(codec, "h264" | "hevc" | "mpeg4"),
        ("mp4", false) => matches!(codec, "aac" | "mp3" | "alac" | "ac3" | "eac3" | "opus"),
        ("mov", false) => matches!(codec, "aac" | "mp3" | "alac" | "ac3" | "eac3"),
        ("webm", true) => matches!(codec, "vp8" | "vp9" | "av1"),
        ("webm", false) => matches!(codec, "opus" | "vorbis"),
        ("mp3", false) => codec == "mp3",
        ("m4a", false) => matches!(codec, "aac" | "alac"),
        ("opus", false) => codec == "opus",
        ("ogg", false) => matches!(codec, "vorbis" | "opus"),
        ("flac", false) => codec == "flac",
        // The WAV muxer takes little-endian PCM and G.711 only.
        ("wav", false) => matches!(
            codec,
            "pcm_s16le"
                | "pcm_s24le"
                | "pcm_s32le"
                | "pcm_f32le"
                | "pcm_f64le"
                | "pcm_u8"
                | "pcm_alaw"
                | "pcm_mulaw"
        ),
        _ => false,
    }
}

/// Extensions FFmpeg can write a copied stream back into. Others (APE, DSF,
/// Musepack…) are read only, so a piece of them is encoded.
pub(crate) const COPY_EXTS: &[&str] = &[
    "mp3", "m4a", "wav", "flac", "ogg", "opus", "mp4", "mkv", "mov", "webm", "aac", "ac3", "eac3",
    "aiff", "aif", "wma", "m4b", "mka", "wv", "tta", "amr", "caf", "mp2", "oga", "w64",
];

/// Demuxers an input may be opened with: common video and sound containers.
/// AviSynth and VapourSynth scripts, playlists, devices and pictures are not
/// among them, so FFmpeg refuses such a file before reading it.
pub(crate) const DEMUXERS: &str = "mov,matroska,avi,asf,flv,mpegts,mpeg,mxf,dv,rm,nut,ogg,wtv,ivf,obu,gif,mp3,wav,w64,flac,aac,ac3,eac3,dts,truehd,mlp,aiff,amr,amrnb,amrwb,ape,wv,tta,dsf,mpc,mpc8,caf,au,voc,loas,xwma,h264,hevc,m4v,mpegvideo";

/// Sound encoder for a format: `has` tells which encoders the build has.
pub(crate) fn audio_codec(ext: &str, has: impl Fn(&str) -> bool) -> Result<Vec<String>, String> {
    let need = |name: &str, what: &str| {
        if has(name) {
            Ok(())
        } else {
            Err(format!("в этой сборке FFmpeg нет кодировщика {what}"))
        }
    };
    Ok(match ext {
        "mp3" => {
            need("libmp3lame", "MP3")?;
            s(&["-c:a", "libmp3lame", "-q:a", "2"])
        }
        "m4a" => s(&["-c:a", "aac", "-b:a", "192k"]),
        "wav" => s(&["-c:a", "pcm_s16le"]),
        "flac" => s(&["-c:a", "flac"]),
        "opus" => {
            need("libopus", "Opus")?;
            s(&["-c:a", "libopus", "-b:a", "128k"])
        }
        "ogg" if has("libvorbis") => s(&["-c:a", "libvorbis", "-q:a", "5"]),
        "ogg" => {
            need("libopus", "Vorbis или Opus")?;
            s(&["-c:a", "libopus", "-b:a", "128k"])
        }
        "webm" => {
            need("libopus", "Opus")?;
            s(&["-c:a", "libopus", "-b:a", "128k"])
        }
        // Sound inside mp4, mov and mkv.
        _ => s(&["-c:a", "aac", "-b:a", "160k"]),
    })
}

/// One FFmpeg command: the common start, inputs, options, one output.
pub(crate) struct Cmd(Vec<String>);

impl Cmd {
    /// Quiet, with progress lines on stdout every half second.
    pub(crate) fn new() -> Self {
        Self::with_level("error")
    }

    /// For passes whose filters print what they found on stderr.
    pub(crate) fn info() -> Self {
        Self::with_level("info")
    }

    fn with_level(level: &str) -> Self {
        Self(s(&[
            "-nostdin",
            "-hide_banner",
            "-nostats",
            "-loglevel",
            level,
            "-progress",
            "pipe:1",
            "-stats_period",
            "0.5",
        ]))
    }

    /// A media file: only the common containers, only the `file` protocol.
    pub(crate) fn input(self, opts: &[String], path: &Path) -> Self {
        self.input_as(DEMUXERS, opts, path)
    }

    /// An input that `formats` may open.
    pub(crate) fn input_as(mut self, formats: &str, opts: &[String], path: &Path) -> Self {
        self.0
            .extend(["-format_whitelist".to_string(), formats.to_string()]);
        self.0.extend(opts.iter().cloned());
        self.0.extend(s(&["-protocol_whitelist", "file", "-i"]));
        self.0.push(format!("file:{}", path.display()));
        self
    }

    pub(crate) fn args(mut self, items: &[String]) -> Self {
        self.0.extend(items.iter().cloned());
        self
    }

    pub(crate) fn arg(mut self, item: impl Into<String>) -> Self {
        self.0.push(item.into());
        self
    }

    /// `-movflags +faststart` for the containers that need it.
    pub(crate) fn faststart(self, ext: &str) -> Self {
        if matches!(ext, "mp4" | "mov" | "m4a") {
            self.args(&s(&["-movflags", "+faststart"]))
        } else {
            self
        }
    }

    pub(crate) fn output(mut self, path: &Path) -> Vec<String> {
        self.0.push("-n".into());
        self.0.push(format!("file:{}", path.display()));
        self.0
    }

    /// Reads everything, writes nothing.
    pub(crate) fn null(mut self) -> Vec<String> {
        self.0.extend(s(&["-f", "null", "-"]));
        self.0
    }
}

/// `-ss` and `-t` before an input.
pub(crate) fn span_opts(start: f64, duration: Option<f64>) -> Vec<String> {
    let mut v = Vec::new();
    if start > 0.0 {
        v.extend(["-ss".into(), arg_time(start)]);
    }
    if let Some(d) = duration {
        v.extend(["-t".into(), arg_time(d)]);
    }
    v
}

/// Change of format: `video` and `audio` are codec arguments, `None` drops
/// that kind of stream.
pub(crate) fn convert_args(
    input: &Path,
    video: Option<&[String]>,
    audio: Option<&[String]>,
    ext: &str,
    out: &Path,
) -> Vec<String> {
    let mut cmd = Cmd::new().input(&[], input);
    cmd = match video {
        Some(v) => cmd.args(v),
        None => cmd.arg("-vn"),
    };
    cmd = match audio {
        Some(a) => cmd.args(a),
        None => cmd.arg("-an"),
    };
    cmd.args(&s(&["-sn", "-dn"])).faststart(ext).output(out)
}

/// The shorter side of the picture down to `h`, the other side in proportion.
pub(crate) fn scale_short(h: u32) -> String {
    format!("scale='if(gt(iw,ih),-2,min({h},iw))':'if(gt(iw,ih),min({h},ih),-2)':flags=lanczos")
}

/// One pass of compression into mp4. `pass` is (1 or 2, log file) for
/// libx264's two passes; the first writes nothing.
pub(crate) fn compress_args(
    input: &Path,
    filter: Option<&str>,
    video: &[String],
    audio: Option<&[String]>,
    pass: Option<(u8, &Path)>,
    out: &Path,
) -> Vec<String> {
    let mut cmd = Cmd::new().input(&[], input);
    if let Some(f) = filter {
        cmd = cmd.arg("-vf").arg(f);
    }
    cmd = cmd.args(video);
    if let Some((n, log)) = pass {
        cmd = cmd
            .arg("-pass")
            .arg(n.to_string())
            .arg("-passlogfile")
            .arg(log.display().to_string());
        if n == 1 {
            return cmd.args(&s(&["-an", "-sn", "-dn", "-f", "null", "NUL"])).0;
        }
    }
    cmd = match audio {
        Some(a) => cmd.args(a),
        None => cmd.arg("-an"),
    };
    cmd.args(&s(&["-sn", "-dn"])).faststart("mp4").output(out)
}

/// A piece by time; `codec` copies or encodes.
pub(crate) fn cut_args(
    input: &Path,
    start: f64,
    duration: f64,
    codec: &[String],
    ext: &str,
    out: &Path,
) -> Vec<String> {
    Cmd::new()
        .input(&span_opts(start, Some(duration)), input)
        .args(codec)
        .faststart(ext)
        .output(out)
}

/// A line of a concat list: `file 'C:/a/b.mp4'`, a quote written as `'\''`.
pub(crate) fn concat_line(path: &Path) -> String {
    let p = path.display().to_string().replace('\\', "/");
    format!("file '{}'", p.replace('\'', "'\\''"))
}

/// Joining files with equal streams without encoding. The list's files are
/// opened under the same whitelists, so `concat` is added to the media ones.
pub(crate) fn join_list_args(list: &Path, ext: &str, out: &Path) -> Vec<String> {
    Cmd::new()
        .input_as(
            &format!("concat,{DEMUXERS}"),
            &s(&["-f", "concat", "-safe", "0"]),
            list,
        )
        .args(&s(&["-c", "copy"]))
        .faststart(ext)
        .output(out)
}

/// One input of a join that encodes.
pub(crate) struct JoinInput<'a> {
    pub path: &'a Path,
    pub has_audio: bool,
    pub duration: f64,
}

/// Joining different files through the concat filter. `size` is the picture
/// of the result, `None` for sound only; `audio_out` is false when no input
/// has sound.
pub(crate) fn join_filter_args(
    inputs: &[JoinInput],
    size: Option<(u32, u32)>,
    audio_out: bool,
    codec: &[String],
    ext: &str,
    out: &Path,
) -> Vec<String> {
    let mut cmd = Cmd::new();
    for i in inputs {
        cmd = cmd.input(&[], i.path);
    }
    let mut graph = Vec::new();
    let mut pads = String::new();
    for (n, i) in inputs.iter().enumerate() {
        if let Some((w, h)) = size {
            graph.push(format!(
                "[{n}:v]scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30,format=yuv420p[v{n}]"
            ));
            pads.push_str(&format!("[v{n}]"));
        }
        if audio_out {
            if i.has_audio {
                graph.push(format!(
                    "[{n}:a]aresample=48000,aformat=channel_layouts=stereo[a{n}]"
                ));
            } else {
                graph.push(format!(
                    "anullsrc=r=48000:cl=stereo,atrim=duration={}[a{n}]",
                    arg_time(i.duration)
                ));
            }
            pads.push_str(&format!("[a{n}]"));
        }
    }
    let v = u8::from(size.is_some());
    let a = u8::from(audio_out);
    let mut tail = format!("{pads}concat=n={}:v={v}:a={a}", inputs.len());
    if v == 1 {
        tail.push_str("[v]");
    }
    if a == 1 {
        tail.push_str("[a]");
    }
    graph.push(tail);
    cmd = cmd.arg("-filter_complex").arg(graph.join(";"));
    if v == 1 {
        cmd = cmd.args(&s(&["-map", "[v]"]));
    }
    if a == 1 {
        cmd = cmd.args(&s(&["-map", "[a]"]));
    }
    cmd.args(codec).faststart(ext).output(out)
}

/// A GIF with its own palette.
pub(crate) fn gif_args(
    input: &Path,
    start: f64,
    duration: f64,
    width: u32,
    fps: u32,
    out: &Path,
) -> Vec<String> {
    Cmd::new()
        .input(&span_opts(start, Some(duration)), input)
        .arg("-vf")
        .arg(format!(
            "fps={fps},scale={width}:-1:flags=lanczos,split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle"
        ))
        .args(&s(&["-loop", "0"]))
        .output(out)
}

/// Turning clockwise by 90, 180 or 270 degrees.
pub(crate) fn transpose(angle: u32) -> &'static str {
    match angle {
        90 => "transpose=1",
        180 => "transpose=1,transpose=1",
        _ => "transpose=2",
    }
}

pub(crate) fn rotate_args(
    input: &Path,
    angle: u32,
    codec: &[String],
    ext: &str,
    out: &Path,
) -> Vec<String> {
    Cmd::new()
        .input(&[], input)
        .arg("-vf")
        .arg(transpose(angle))
        .args(codec)
        .args(&s(&["-sn", "-dn"]))
        .faststart(ext)
        .output(out)
}

/// `atempo` steps within 0.5..2 whose product is `factor`.
pub(crate) fn atempo_chain(factor: f64) -> String {
    let mut left = factor;
    let mut steps = Vec::new();
    while left > 2.0 + 1e-9 {
        steps.push("atempo=2".to_string());
        left /= 2.0;
    }
    while left < 0.5 - 1e-9 {
        steps.push("atempo=0.5".to_string());
        left /= 0.5;
    }
    if (left - 1.0).abs() > 1e-9 || steps.is_empty() {
        steps.push(format!("atempo={}", number(left)));
    }
    steps.join(",")
}

/// A short decimal: `2`, `0.5`, `1.25`.
pub(crate) fn number(x: f64) -> String {
    let s = format!("{x:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

pub(crate) fn speed_args(
    input: &Path,
    factor: f64,
    video: bool,
    audio: bool,
    codec: &[String],
    ext: &str,
    out: &Path,
) -> Vec<String> {
    let mut graph = Vec::new();
    if video {
        graph.push(format!("[0:v]setpts=PTS/{}[v]", number(factor)));
    }
    if audio {
        graph.push(format!("[0:a]{}[a]", atempo_chain(factor)));
    }
    let mut cmd = Cmd::new()
        .input(&[], input)
        .arg("-filter_complex")
        .arg(graph.join(";"));
    if video {
        cmd = cmd.args(&s(&["-map", "[v]"]));
    }
    if audio {
        cmd = cmd.args(&s(&["-map", "[a]"]));
    }
    cmd.args(codec).faststart(ext).output(out)
}

/// A pass that only looks for silence.
pub(crate) fn silence_args(input: &Path, noise: &str, min_s: &str) -> Vec<String> {
    Cmd::info()
        .input(&[], input)
        .args(&s(&["-vn", "-sn", "-dn", "-af"]))
        .arg(format!("silencedetect=noise={noise}:d={min_s}"))
        .null()
}

/// Noise reduction strength.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Denoise {
    Off,
    Normal,
    Strong,
}

/// Change of loudness.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Gain {
    Db(f64),
    Times(f64),
}

impl Gain {
    pub(crate) fn louder(self) -> bool {
        match self {
            Gain::Db(d) => d > 0.0,
            Gain::Times(t) => t > 1.0,
        }
    }
}

/// Filters before loudness normalisation, in order.
pub(crate) fn audio_filters(denoise: Denoise, gain: Option<Gain>) -> Vec<String> {
    let mut v = Vec::new();
    match denoise {
        Denoise::Off => {}
        Denoise::Normal => v.push("highpass=f=80,afftdn=nr=12:nf=-50:tn=1".to_string()),
        Denoise::Strong => v.push("highpass=f=80,afftdn=nr=24:nf=-40:tn=1".to_string()),
    }
    if let Some(g) = gain {
        v.push(match g {
            Gain::Db(d) => format!("volume={}dB", number(d)),
            Gain::Times(t) => format!("volume={}", number(t)),
        });
        if g.louder() {
            v.push("alimiter=limit=0.95".into());
        }
    }
    v
}

const LOUDNORM: &str = "loudnorm=I=-16:TP=-1.5:LRA=11";

/// First pass of loudness normalisation: it only measures.
pub(crate) fn loudnorm_measure_args(
    input: &Path,
    opts: &[String],
    filters: &[String],
) -> Vec<String> {
    let mut chain = filters.to_vec();
    chain.push(format!("{LOUDNORM}:print_format=json"));
    Cmd::info()
        .input(opts, input)
        .args(&s(&["-vn", "-sn", "-dn", "-af"]))
        .arg(chain.join(","))
        .null()
}

/// The sound pass that writes the result. With `measured`, the second pass
/// of loudness normalisation.
pub(crate) fn audio_pass_args(
    input: &Path,
    opts: &[String],
    filters: &[String],
    measured: Option<&Loudness>,
    codec: &[String],
    ext: &str,
    out: &Path,
) -> Vec<String> {
    let mut chain = filters.to_vec();
    if let Some(m) = measured {
        chain.push(format!(
            "{LOUDNORM}:measured_I={}:measured_TP={}:measured_LRA={}:measured_thresh={}:offset={}:linear=true:print_format=summary",
            m.input_i, m.input_tp, m.input_lra, m.input_thresh, m.target_offset
        ));
    }
    let mut cmd = Cmd::new().input(opts, input);
    if !chain.is_empty() {
        cmd = cmd.arg("-af").arg(chain.join(","));
    }
    if measured.is_some() {
        cmd = cmd.args(&s(&["-ar", "48000"]));
    }
    cmd.args(codec)
        .args(&s(&["-sn", "-dn"]))
        .faststart(ext)
        .output(out)
}

/// The `subtitles` filter for a file in the working folder.
pub(crate) fn subtitles_filter(name: &str, ext: &str, font_size: u32) -> String {
    if matches!(ext, "ass" | "ssa") {
        format!("subtitles=filename={name}")
    } else {
        format!(
            "subtitles=filename={name}:charenc=UTF-8:force_style='FontName=Segoe UI,FontSize={font_size}'"
        )
    }
}

pub(crate) fn subtitles_args(
    input: &Path,
    filter: &str,
    codec: &[String],
    out: &Path,
) -> Vec<String> {
    Cmd::new()
        .input(&[], input)
        .arg("-vf")
        .arg(filter)
        .args(codec)
        .args(&s(&["-sn", "-dn"]))
        .faststart("mp4")
        .output(out)
}

/// The sound for recognition: 16 kHz mono WAV of at most `max_s` seconds,
/// the length that was asked about.
pub(crate) fn wav_args(input: &Path, max_s: f64, out: &Path) -> Vec<String> {
    Cmd::new()
        .input(&span_opts(0.0, Some(max_s)), input)
        .args(&s(&[
            "-vn",
            "-sn",
            "-dn",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-c:a",
            "pcm_s16le",
        ]))
        .output(out)
}

/// One frame at `t` seconds as PNG, at most 768 pixels wide.
pub(crate) fn frame_args(input: &Path, t: f64, out: &Path) -> Vec<String> {
    Cmd::new()
        .input(&["-ss".into(), arg_time(t)], input)
        .args(&s(&[
            "-frames:v",
            "1",
            "-vf",
            "scale='min(768,iw)':-2",
            "-update",
            "1",
            "-c:v",
            "png",
        ]))
        .output(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREFIX: &str =
        "-nostdin -hide_banner -nostats -loglevel error -progress pipe:1 -stats_period 0.5";

    fn line(v: &[String]) -> String {
        v.join(" ")
    }

    fn formats() -> String {
        format!("-format_whitelist {DEMUXERS}")
    }

    fn p(s: &str) -> &Path {
        Path::new(s)
    }

    #[test]
    fn convert_reads_one_local_file_and_never_overwrites() {
        let v = s(&["-c:v", "copy"]);
        let a = s(&["-c:a", "aac", "-b:a", "160k"]);
        assert_eq!(
            line(&convert_args(p("C:\\v\\a.webm"), Some(&v), Some(&a), "mp4", p("C:\\v\\a.mp4"))),
            format!("{PREFIX} {} -protocol_whitelist file -i file:C:\\v\\a.webm -c:v copy -c:a aac -b:a 160k -sn -dn -movflags +faststart -n file:C:\\v\\a.mp4", formats())
        );
        let mp3 = s(&["-c:a", "libmp3lame", "-q:a", "2"]);
        assert_eq!(
            line(&convert_args(p("a.wav"), None, Some(&mp3), "mp3", p("a.mp3"))),
            format!("{PREFIX} {} -protocol_whitelist file -i file:a.wav -vn -c:a libmp3lame -q:a 2 -sn -dn -n file:a.mp3", formats())
        );
    }

    #[test]
    fn inputs_open_only_media_demuxers() {
        let names: Vec<&str> = DEMUXERS.split(',').collect();
        for need in ["mov", "matroska", "avi", "mp3", "wav", "flac", "ogg", "asf"] {
            assert!(names.contains(&need), "{need}");
        }
        for never in [
            "avisynth",
            "vapoursynth",
            "hls",
            "dash",
            "concat",
            "sdp",
            "image2",
            "lavfi",
        ] {
            assert!(!names.contains(&never), "{never}");
        }
        let l = line(&join_list_args(p("list.txt"), "mp4", p("o.mp4")));
        assert!(l.contains(&format!("-format_whitelist concat,{DEMUXERS} -f concat")));
    }

    #[test]
    fn copy_table_and_sound_encoders() {
        assert!(fits("mp4", "hevc", true) && fits("mp4", "opus", false));
        assert!(!fits("mov", "opus", false) && !fits("mp4", "vp9", true));
        assert!(fits("webm", "vp9", true) && !fits("webm", "aac", false));
        assert!(fits("mkv", "anything", true) && fits("wav", "pcm_s24le", false));
        assert!(!fits("wav", "pcm_s16be", false) && !fits("wav", "pcm_bluray", false));
        assert!(fits("mp4", "av1", true) && !fits("mov", "av1", true));
        assert!(fits("m4a", "alac", false) && !fits("mp3", "aac", false));
        let all = |_: &str| true;
        let none = |_: &str| false;
        assert_eq!(
            line(&audio_codec("ogg", all).unwrap()),
            "-c:a libvorbis -q:a 5"
        );
        assert_eq!(
            line(&audio_codec("ogg", |e: &str| e == "libopus").unwrap()),
            "-c:a libopus -b:a 128k"
        );
        assert_eq!(
            line(&audio_codec("m4a", none).unwrap()),
            "-c:a aac -b:a 192k"
        );
        assert_eq!(
            line(&audio_codec("mkv", none).unwrap()),
            "-c:a aac -b:a 160k"
        );
        assert_eq!(
            audio_codec("mp3", none),
            Err("в этой сборке FFmpeg нет кодировщика MP3".into())
        );
        assert_eq!((video_ext("avi"), video_ext("webm")), ("mp4", "webm"));
        assert_eq!((audio_ext("aac"), audio_ext("flac")), ("m4a", "flac"));
    }

    #[test]
    fn compress_two_passes_and_scale() {
        assert_eq!(
            scale_short(720),
            "scale='if(gt(iw,ih),-2,min(720,iw))':'if(gt(iw,ih),min(720,ih),-2)':flags=lanczos"
        );
        let video = s(&["-c:v", "libx264", "-preset", "medium", "-b:v", "900k"]);
        let audio = s(&["-c:a", "aac", "-b:a", "128k"]);
        let log = p("C:\\T\\mika2p");
        let one = compress_args(
            p("in.mp4"),
            None,
            &video,
            Some(&audio),
            Some((1, log)),
            p("o.mp4"),
        );
        assert!(line(&one)
            .ends_with("-b:v 900k -pass 1 -passlogfile C:\\T\\mika2p -an -sn -dn -f null NUL"));
        let two = compress_args(
            p("in.mp4"),
            Some("scale=x"),
            &video,
            Some(&audio),
            Some((2, log)),
            p("o.mp4"),
        );
        assert!(line(&two).contains("file:in.mp4 -vf scale=x -c:v libx264"));
        assert!(line(&two).ends_with(
            "-pass 2 -passlogfile C:\\T\\mika2p -c:a aac -b:a 128k -sn -dn -movflags +faststart -n file:o.mp4"
        ));
        let silent = compress_args(p("in.mp4"), None, &video, None, None, p("o.mp4"));
        assert!(line(&silent).contains("-b:v 900k -an -sn -dn"));
    }

    #[test]
    fn cut_puts_the_span_before_the_input() {
        assert_eq!(
            line(&cut_args(p("a.mp3"), 90.0, 30.0, &s(&["-c", "copy"]), "mp3", p("b.mp3"))),
            format!("{PREFIX} {} -ss 90.000 -t 30.000 -protocol_whitelist file -i file:a.mp3 -c copy -n file:b.mp3", formats())
        );
        assert_eq!(line(&span_opts(0.0, Some(5.0))), "-t 5.000");
    }

    #[test]
    fn join_lists_and_filters() {
        assert_eq!(
            concat_line(p("C:\\a\\it's.mp4")),
            "file 'C:/a/it'\\''s.mp4'"
        );
        let l = line(&join_list_args(p("C:\\T\\list.txt"), "mp4", p("o.mp4")));
        assert!(l.contains("-f concat -safe 0 -protocol_whitelist file -i file:C:\\T\\list.txt -c copy -movflags +faststart -n file:o.mp4"));

        let inputs = [
            JoinInput {
                path: p("a.mp4"),
                has_audio: true,
                duration: 10.0,
            },
            JoinInput {
                path: p("b.mp4"),
                has_audio: false,
                duration: 4.5,
            },
        ];
        let v = join_filter_args(
            &inputs,
            Some((1280, 720)),
            true,
            &s(&["-c:v", "libx264"]),
            "mp4",
            p("o.mp4"),
        );
        let graph = &v[v.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        assert_eq!(
            graph,
            "[0:v]scale=1280:720:force_original_aspect_ratio=decrease,pad=1280:720:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30,format=yuv420p[v0];\
[0:a]aresample=48000,aformat=channel_layouts=stereo[a0];\
[1:v]scale=1280:720:force_original_aspect_ratio=decrease,pad=1280:720:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30,format=yuv420p[v1];\
anullsrc=r=48000:cl=stereo,atrim=duration=4.500[a1];\
[v0][a0][v1][a1]concat=n=2:v=1:a=1[v][a]"
        );
        assert!(line(&v).contains("-map [v] -map [a] -c:v libx264"));
        assert_eq!(v.iter().filter(|a| a.starts_with("file:")).count(), 3);

        let sound = join_filter_args(
            &inputs[..1],
            None,
            true,
            &s(&["-c:a", "flac"]),
            "flac",
            p("o.flac"),
        );
        let l = line(&sound);
        assert!(
            l.contains("[a0]concat=n=1:v=0:a=1[a] -map [a] -c:a flac -n file:o.flac"),
            "{l}"
        );
    }

    #[test]
    fn gif_rotate_and_speed() {
        let g = gif_args(p("a.mp4"), 3.0, 5.0, 480, 12, p("a.gif"));
        assert!(line(&g).contains("-ss 3.000 -t 5.000 -protocol_whitelist file -i file:a.mp4 -vf fps=12,scale=480:-1:flags=lanczos,split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle -loop 0 -n file:a.gif"));
        assert_eq!(transpose(90), "transpose=1");
        assert_eq!(transpose(180), "transpose=1,transpose=1");
        assert_eq!(transpose(270), "transpose=2");
        assert!(line(&rotate_args(p("a.mkv"), 270, &[], "mkv", p("b.mkv")))
            .contains("-vf transpose=2 -sn -dn -n"));

        assert_eq!(atempo_chain(4.0), "atempo=2,atempo=2");
        assert_eq!(atempo_chain(0.25), "atempo=0.5,atempo=0.5");
        assert_eq!(atempo_chain(1.5), "atempo=1.5");
        assert_eq!(atempo_chain(3.0), "atempo=2,atempo=1.5");
        assert_eq!(atempo_chain(2.0), "atempo=2");
        let v = speed_args(p("a.mp4"), 2.0, true, true, &[], "mp4", p("b.mp4"));
        assert!(line(&v)
            .contains("-filter_complex [0:v]setpts=PTS/2[v];[0:a]atempo=2[a] -map [v] -map [a]"));
        let a = speed_args(p("a.mp3"), 0.5, false, true, &[], "mp3", p("b.mp3"));
        assert!(line(&a).contains("-filter_complex [0:a]atempo=0.5[a] -map [a] -n"));
        let mute = speed_args(p("a.mp4"), 0.5, true, false, &[], "mp4", p("b.mp4"));
        assert!(!line(&mute).contains("atempo"));
    }

    #[test]
    fn sound_filters_in_order_and_loudnorm_passes() {
        assert_eq!(
            audio_filters(Denoise::Normal, Some(Gain::Db(6.0))),
            vec![
                "highpass=f=80,afftdn=nr=12:nf=-50:tn=1",
                "volume=6dB",
                "alimiter=limit=0.95"
            ]
        );
        assert_eq!(
            audio_filters(Denoise::Strong, Some(Gain::Times(0.5))),
            vec!["highpass=f=80,afftdn=nr=24:nf=-40:tn=1", "volume=0.5"]
        );
        let m = loudnorm_measure_args(p("a.wav"), &span_opts(1.0, None), &["volume=2".into()]);
        let l = line(&m);
        assert!(l.contains("-loglevel info"));
        assert!(l.contains("-ss 1.000 -protocol_whitelist file -i file:a.wav"));
        assert!(
            l.ends_with("-af volume=2,loudnorm=I=-16:TP=-1.5:LRA=11:print_format=json -f null -")
        );

        let measured = Loudness {
            input_i: "-27.61".into(),
            input_tp: "-4.47".into(),
            input_lra: "18.06".into(),
            input_thresh: "-39.20".into(),
            target_offset: "0.58".into(),
        };
        let two = audio_pass_args(
            p("a.wav"),
            &[],
            &[],
            Some(&measured),
            &s(&["-c:a", "pcm_s16le"]),
            "wav",
            p("b.wav"),
        );
        assert!(line(&two).contains("-af loudnorm=I=-16:TP=-1.5:LRA=11:measured_I=-27.61:measured_TP=-4.47:measured_LRA=18.06:measured_thresh=-39.20:offset=0.58:linear=true:print_format=summary -ar 48000 -c:a pcm_s16le"));
        let plain = audio_pass_args(
            p("a.mp4"),
            &[],
            &[],
            None,
            &s(&["-c:v", "copy"]),
            "mp4",
            p("b.mp4"),
        );
        assert!(!line(&plain).contains("-af"));
        let sil = silence_args(p("a.wav"), "-50dB", "0.3");
        assert!(line(&sil).ends_with("-vn -sn -dn -af silencedetect=noise=-50dB:d=0.3 -f null -"));
    }

    #[test]
    fn subtitles_frames_and_wav() {
        assert_eq!(
            subtitles_filter("mika_subs.srt", "srt", 22),
            "subtitles=filename=mika_subs.srt:charenc=UTF-8:force_style='FontName=Segoe UI,FontSize=22'"
        );
        assert_eq!(
            subtitles_filter("mika_subs.ass", "ass", 22),
            "subtitles=filename=mika_subs.ass"
        );
        let f = frame_args(p("a.mp4"), 12.5, p("C:\\T\\frame-01.png"));
        assert!(line(&f).contains("-ss 12.500 -protocol_whitelist file -i file:a.mp4 -frames:v 1 -vf scale='min(768,iw)':-2 -update 1 -c:v png -n file:C:\\T\\frame-01.png"));
        let w = wav_args(p("a.mp4"), 301.0, p("C:\\T\\audio.wav"));
        assert!(line(&w).ends_with(
            "-t 301.000 -protocol_whitelist file -i file:a.mp4 -vn -sn -dn -ac 1 -ar 16000 -c:a pcm_s16le -n file:C:\\T\\audio.wav"
        ));
        assert_eq!(
            (number(2.0), number(0.5), number(1.25)),
            ("2".into(), "0.5".into(), "1.25".into())
        );
    }
}
