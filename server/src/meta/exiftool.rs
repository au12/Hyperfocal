//! A persistent ExifTool process (`-stay_open`) used for every XMP sidecar write.
//!
//! ExifTool is the reference implementation for XMP: it rewrites only the tags it is
//! asked to change, keeps every other namespace intact (Lightroom develop settings,
//! Excire data, darktable history...) and writes through a temporary file, so a
//! failed write never leaves a half-written sidecar behind.

use anyhow::{Context, Result, anyhow, bail};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};

/// ExifTool does not know `xmpDM:pick` / `xmpDM:good` in every version; Lightroom
/// Classic 13.2+ stores pick/reject flags there. Defining them here makes the
/// behaviour identical on any ExifTool version.
pub const CONFIG: &str = r#"%Image::ExifTool::UserDefined = (
    'Image::ExifTool::XMP::xmpDM' => {
        pick => { Writable => 'integer' },
        good => { Writable => 'boolean' },
    },
);
1;
"#;

pub struct ExifTool {
    bin: String,
    config: PathBuf,
    proc: Option<Proc>,
    seq: u64,
}

struct Proc {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    stderr: Lines<BufReader<ChildStderr>>,
}

#[derive(Debug)]
pub struct ExecOutput {
    pub stdout: String,
    pub stderr: String,
}

impl ExecOutput {
    /// Number of files ExifTool reports as updated or created.
    pub fn changed(&self) -> u32 {
        let mut n = 0;
        for line in self.stdout.lines() {
            let l = line.trim();
            for suffix in ["image files updated", "image files created"] {
                if let Some(num) = l.strip_suffix(suffix) {
                    n += num.trim().parse::<u32>().unwrap_or(0);
                }
            }
        }
        n
    }
    pub fn error(&self) -> Option<String> {
        self.stderr
            .lines()
            .find(|l| l.starts_with("Error"))
            .map(|l| l.to_string())
            .or_else(|| self.stdout.lines().find(|l| l.contains("weren't updated due to errors")).map(|s| s.to_string()))
    }
}

impl ExifTool {
    pub fn new(bin: &str, config_dir: &Path) -> Result<ExifTool> {
        let config = config_dir.join("exiftool.config");
        std::fs::write(&config, CONFIG).with_context(|| format!("writing {}", config.display()))?;
        Ok(ExifTool { bin: bin.to_string(), config, proc: None, seq: 0 })
    }

    async fn spawn(&mut self) -> Result<()> {
        let mut child = Command::new(&self.bin)
            .arg("-config")
            .arg(&self.config)
            .args(["-stay_open", "True", "-@", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("starting {}", self.bin))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = BufReader::new(child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?).lines();
        let stderr = BufReader::new(child.stderr.take().ok_or_else(|| anyhow!("no stderr"))?).lines();
        self.proc = Some(Proc { child, stdin, stdout, stderr });
        Ok(())
    }

    /// Run one ExifTool command. Arguments may not contain newlines.
    pub async fn exec(&mut self, args: &[String]) -> Result<ExecOutput> {
        for a in args {
            if a.contains('\n') || a.contains('\r') {
                bail!("argument contains a line break");
            }
        }
        if self.proc.is_none() {
            self.spawn().await?;
        }
        self.seq += 1;
        let marker = format!("{{ready{}}}", self.seq);
        let result = tokio::time::timeout(Duration::from_secs(120), async {
            let p = self.proc.as_mut().unwrap();
            let mut input = String::new();
            for a in args {
                input.push_str(a);
                input.push('\n');
            }
            input.push_str(&format!("-echo4\n{marker}\n-execute{}\n", self.seq));
            p.stdin.write_all(input.as_bytes()).await?;
            p.stdin.flush().await?;
            let mut out = String::new();
            loop {
                match p.stdout.next_line().await? {
                    Some(l) if l.trim() == marker => break,
                    Some(l) => {
                        out.push_str(&l);
                        out.push('\n');
                    }
                    None => bail!("exiftool exited"),
                }
            }
            let mut err = String::new();
            loop {
                match p.stderr.next_line().await? {
                    Some(l) if l.trim() == marker => break,
                    Some(l) => {
                        err.push_str(&l);
                        err.push('\n');
                    }
                    None => bail!("exiftool exited"),
                }
            }
            Ok::<_, anyhow::Error>(ExecOutput { stdout: out, stderr: err })
        })
        .await;
        match result {
            Ok(Ok(o)) => Ok(o),
            Ok(Err(e)) => {
                self.kill().await;
                Err(e)
            }
            Err(_) => {
                self.kill().await;
                bail!("exiftool timed out")
            }
        }
    }

    async fn kill(&mut self) {
        if let Some(mut p) = self.proc.take() {
            let _ = p.child.kill().await;
        }
    }

    pub async fn version(&mut self) -> Result<String> {
        let o = self.exec(&["-ver".to_string()]).await?;
        Ok(o.stdout.trim().to_string())
    }
}

impl Drop for ExifTool {
    fn drop(&mut self) {
        if let Some(p) = self.proc.as_mut() {
            let _ = p.child.start_kill();
        }
    }
}
