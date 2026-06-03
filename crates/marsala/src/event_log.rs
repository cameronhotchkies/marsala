use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

#[derive(Debug, Clone)]
pub struct EventLogHandle {
    sender: Option<mpsc::UnboundedSender<EventRecord>>,
}

impl EventLogHandle {
    pub fn disabled() -> Self {
        Self { sender: None }
    }

    pub fn emit(&self, event_type: impl Into<String>, data: Value) {
        let Some(sender) = &self.sender else {
            return;
        };

        let record = EventRecord {
            timestamp: Utc::now(),
            event_type: event_type.into(),
            data,
        };

        if let Err(error) = sender.send(record) {
            warn!(%error, "failed to enqueue event log record");
        }
    }
}

pub struct EventLogWriter {
    sender: Option<mpsc::UnboundedSender<EventRecord>>,
    task: Option<JoinHandle<Result<()>>>,
}

impl EventLogWriter {
    pub async fn spawn(path: &Path, enabled: bool) -> Result<Self> {
        if !enabled {
            return Ok(Self {
                sender: None,
                task: None,
            });
        }

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        let (sender, mut receiver) = mpsc::unbounded_channel::<EventRecord>();
        let path = path.to_path_buf();

        let task = tokio::spawn(async move {
            let mut writer = BufWriter::new(file);

            while let Some(record) = receiver.recv().await {
                serde_json::to_writer(&mut writer, &record)
                    .with_context(|| format!("failed to serialize event for {}", path.display()))?;
                writer.write_all(b"\n")?;
                writer.flush()?;
            }

            writer.flush()?;
            Ok(())
        });

        Ok(Self {
            sender: Some(sender),
            task: Some(task),
        })
    }

    pub fn handle(&self) -> EventLogHandle {
        EventLogHandle {
            sender: self.sender.clone(),
        }
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        self.sender.take();

        if let Some(task) = self.task.take() {
            task.await.context("event log task panicked")??;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRecord {
    pub timestamp: DateTime<Utc>,
    pub event_type: String,
    pub data: Value,
}

pub fn read_tail_lines(path: &Path, lines: usize) -> Result<Vec<String>> {
    Ok(read_tail_snapshot(path, lines)?.lines)
}

pub async fn tail_log_file(path: &Path, lines: usize, follow: bool) -> Result<()> {
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let mut stdout = stdout.lock();
    let mut stderr = stderr.lock();

    let tail_snapshot = read_tail_snapshot(path, lines)?;

    for line in &tail_snapshot.lines {
        writeln!(stdout, "{line}")?;
    }
    stdout.flush()?;

    if !follow {
        return Ok(());
    }

    follow_log_file(
        path.to_path_buf(),
        tail_snapshot.follow_start,
        &mut stdout,
        &mut stderr,
    )
    .await
}

async fn follow_log_file(
    path: PathBuf,
    follow_start: FollowStart,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<()> {
    let mut follower = LogFollower::with_follow_start(path, follow_start);

    loop {
        let events = follower.poll()?;

        for event in events {
            match event {
                FollowEvent::Line(line) => {
                    write!(stdout, "{line}")?;
                    stdout.flush()?;
                }
                FollowEvent::WaitingForFile(path) => {
                    writeln!(stderr, "waiting for log file: {}", path.display())?;
                    stderr.flush()?;
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FollowEvent {
    Line(String),
    WaitingForFile(PathBuf),
}

struct LogFollower {
    path: PathBuf,
    reader: Option<BufReader<File>>,
    cursor: u64,
    current_file_id: Option<FileId>,
    report_waiting: bool,
    initial_open: Option<InitialOpen>,
}

impl LogFollower {
    fn new(path: PathBuf, start_at_end_on_first_open: bool) -> Self {
        let initial_open = start_at_end_on_first_open.then_some(InitialOpen::StartAtEnd);
        Self::with_initial_open(path, initial_open)
    }

    fn with_follow_start(path: PathBuf, follow_start: FollowStart) -> Self {
        let initial_open = match follow_start {
            FollowStart::StartAtBeginning => None,
            FollowStart::ResumeFromSnapshot { file_id, cursor } => {
                Some(InitialOpen::ResumeFromSnapshot { file_id, cursor })
            }
        };
        Self::with_initial_open(path, initial_open)
    }

    fn with_initial_open(path: PathBuf, initial_open: Option<InitialOpen>) -> Self {
        Self {
            path,
            reader: None,
            cursor: 0,
            current_file_id: None,
            report_waiting: true,
            initial_open,
        }
    }

    fn poll(&mut self) -> Result<Vec<FollowEvent>> {
        let mut events = Vec::new();

        loop {
            if self.reader.is_none() {
                match self.open_current_path()? {
                    Some((mut reader, file_state)) => {
                        self.cursor = match self.initial_open.take() {
                            Some(InitialOpen::StartAtEnd) => file_state.len,
                            Some(InitialOpen::ResumeFromSnapshot { file_id, cursor })
                                if file_state.id == file_id =>
                            {
                                cursor
                            }
                            Some(InitialOpen::ResumeFromSnapshot { .. }) | None => 0,
                        };
                        if self.cursor > 0 {
                            reader.seek(SeekFrom::Start(self.cursor))?;
                        }

                        self.reader = Some(reader);
                        self.current_file_id = Some(file_state.id);
                        self.report_waiting = true;
                    }
                    None => {
                        self.initial_open = None;
                        if self.report_waiting {
                            events.push(FollowEvent::WaitingForFile(self.path.clone()));
                            self.report_waiting = false;
                        }
                        return Ok(events);
                    }
                }
            }

            let mut line = String::new();
            let read = {
                let reader = self.reader.as_mut().expect("reader must be open");
                reader.read_line(&mut line)?
            };
            if read > 0 {
                self.cursor += read as u64;
                events.push(FollowEvent::Line(line));
                continue;
            }

            match FileState::from_path(&self.path)? {
                Some(path_file_state) => {
                    if Some(path_file_state.id) != self.current_file_id {
                        debug!("log file replaced, reopening current path");
                        self.reader = None;
                        self.current_file_id = None;
                        self.cursor = 0;
                        continue;
                    }

                    if path_file_state.len < self.cursor {
                        debug!("log file truncated, rewinding");
                        self.cursor = 0;
                        let reader = self.reader.as_mut().expect("reader must be open");
                        reader.seek(SeekFrom::Start(0))?;
                        continue;
                    }

                    return Ok(events);
                }
                None => {
                    debug!("log file missing, waiting for recreation");
                    self.reader = None;
                    self.current_file_id = None;
                    self.cursor = 0;
                    if self.report_waiting {
                        events.push(FollowEvent::WaitingForFile(self.path.clone()));
                        self.report_waiting = false;
                    }
                    return Ok(events);
                }
            }
        }
    }

    fn open_current_path(&self) -> Result<Option<(BufReader<File>, FileState)>> {
        match File::open(&self.path) {
            Ok(file) => {
                let file_state = FileState::from_file(&file)?;
                Ok(Some((BufReader::new(file), file_state)))
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => {
                Err(error).with_context(|| format!("failed to open {}", self.path.display()))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FollowStart {
    StartAtBeginning,
    ResumeFromSnapshot { file_id: FileId, cursor: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TailSnapshot {
    lines: Vec<String>,
    follow_start: FollowStart,
}

fn read_tail_snapshot(path: &Path, lines: usize) -> Result<TailSnapshot> {
    match File::open(path) {
        Ok(file) => {
            let file_state = FileState::from_file(&file)?;
            let mut reader = BufReader::new(file);
            let mut content = String::new();
            reader
                .read_to_string(&mut content)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let all_lines: Vec<_> = content.lines().map(ToOwned::to_owned).collect();
            let start = all_lines.len().saturating_sub(lines);
            Ok(TailSnapshot {
                lines: all_lines[start..].to_vec(),
                follow_start: FollowStart::ResumeFromSnapshot {
                    file_id: file_state.id,
                    cursor: content.len() as u64,
                },
            })
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(TailSnapshot {
            lines: Vec::new(),
            follow_start: FollowStart::StartAtBeginning,
        }),
        Err(error) => Err(error).with_context(|| format!("failed to open {}", path.display())),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InitialOpen {
    StartAtEnd,
    ResumeFromSnapshot { file_id: FileId, cursor: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileState {
    len: u64,
    id: FileId,
}

impl FileState {
    fn from_file(file: &File) -> Result<Self> {
        let metadata = file.metadata().context("failed to read file metadata")?;
        Ok(Self::from_metadata(metadata))
    }

    fn from_path(path: &Path) -> Result<Option<Self>> {
        match fs::metadata(path) {
            Ok(metadata) => Ok(Some(Self::from_metadata(metadata))),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("failed to stat {}", path.display())),
        }
    }

    fn from_metadata(metadata: fs::Metadata) -> Self {
        Self {
            len: metadata.len(),
            id: FileId::from_metadata(&metadata),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileId {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(not(unix))]
    modified: Option<std::time::SystemTime>,
}

impl FileId {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            Self {
                dev: metadata.dev(),
                ino: metadata.ino(),
            }
        }

        #[cfg(not(unix))]
        {
            Self {
                modified: metadata.modified().ok(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn writer_persists_jsonl_events() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("events.jsonl");
        let mut writer = EventLogWriter::spawn(&path, true).await.expect("writer");
        let handle = writer.handle();

        handle.emit("test_event", json!({ "ok": true }));
        drop(handle);
        writer.shutdown().await.expect("shutdown");

        let lines = read_tail_lines(&path, 10).expect("tail");
        assert_eq!(lines.len(), 1);

        let record: EventRecord = serde_json::from_str(&lines[0]).expect("json line");
        assert_eq!(record.event_type, "test_event");
        assert_eq!(record.data["ok"], json!(true));
    }

    #[test]
    fn tail_reads_last_n_lines() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("events.jsonl");
        fs::write(&path, "one\ntwo\nthree\n").expect("write log");

        let lines = read_tail_lines(&path, 2).expect("tail");
        assert_eq!(lines, vec!["two".to_string(), "three".to_string()]);
    }

    #[test]
    fn tail_missing_file_returns_no_lines() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("missing.jsonl");

        let lines = read_tail_lines(&path, 20).expect("tail");
        assert!(lines.is_empty());
    }

    #[test]
    fn follower_waits_once_and_reads_new_file_from_start() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("events.jsonl");
        let mut follower = LogFollower::new(path.clone(), true);

        assert_eq!(
            follower.poll().expect("initial poll"),
            vec![FollowEvent::WaitingForFile(path.clone())]
        );
        assert!(follower.poll().expect("repeat poll").is_empty());

        fs::write(&path, "first\n").expect("write new log");
        assert_eq!(
            follower.poll().expect("poll after creation"),
            vec![FollowEvent::Line("first\n".to_string())]
        );
    }

    #[test]
    fn follower_starts_at_end_for_existing_file_and_reads_appends() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("events.jsonl");
        fs::write(&path, "old\n").expect("write initial log");

        let mut follower = LogFollower::new(path.clone(), true);
        assert!(follower.poll().expect("initial poll").is_empty());

        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open log for append");
        file.write_all(b"new\n").expect("append log");

        assert_eq!(
            follower.poll().expect("poll after append"),
            vec![FollowEvent::Line("new\n".to_string())]
        );
    }

    #[test]
    fn follower_uses_tail_snapshot_cursor_for_same_file() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("events.jsonl");
        fs::write(&path, "old\n").expect("write initial log");

        let snapshot = read_tail_snapshot(&path, 10).expect("tail snapshot");
        assert_eq!(snapshot.lines, vec!["old".to_string()]);

        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open log for append");
        file.write_all(b"new\n").expect("append log");

        let mut follower = LogFollower::with_follow_start(path.clone(), snapshot.follow_start);
        assert_eq!(
            follower.poll().expect("poll after append"),
            vec![FollowEvent::Line("new\n".to_string())]
        );
    }

    #[test]
    fn follower_detects_renamed_and_recreated_path() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("events.jsonl");
        let rotated = tempdir.path().join("events.jsonl.1");
        fs::write(&path, "old\n").expect("write initial log");

        let mut follower = LogFollower::new(path.clone(), true);
        assert!(follower.poll().expect("initial poll").is_empty());

        fs::rename(&path, &rotated).expect("rotate log");
        fs::write(&path, "new\n").expect("write replacement log");

        assert_eq!(
            follower.poll().expect("poll after recreation"),
            vec![FollowEvent::Line("new\n".to_string())]
        );
    }

    #[test]
    fn follower_reads_replacement_from_start_when_path_changes_before_first_poll() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("events.jsonl");
        let rotated = tempdir.path().join("events.jsonl.1");
        fs::write(&path, "old\n").expect("write initial log");

        let snapshot = read_tail_snapshot(&path, 10).expect("tail snapshot");
        fs::rename(&path, &rotated).expect("rotate log");
        fs::write(&path, "new\n").expect("write replacement log");

        let mut follower = LogFollower::with_follow_start(path.clone(), snapshot.follow_start);
        assert_eq!(
            follower.poll().expect("poll replacement"),
            vec![FollowEvent::Line("new\n".to_string())]
        );
    }

    #[test]
    fn follower_rewinds_after_truncate() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("events.jsonl");
        fs::write(&path, "one\ntwo\n").expect("write initial log");

        let mut follower = LogFollower::new(path.clone(), true);
        assert!(follower.poll().expect("initial poll").is_empty());

        fs::write(&path, "reset\n").expect("truncate and rewrite");

        assert_eq!(
            follower.poll().expect("poll after truncate"),
            vec![FollowEvent::Line("reset\n".to_string())]
        );
    }

    #[test]
    fn follower_reads_new_file_from_start_when_initial_tail_saw_no_file() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("events.jsonl");

        let snapshot = read_tail_snapshot(&path, 10).expect("tail snapshot");
        assert!(snapshot.lines.is_empty());
        fs::write(&path, "first\n").expect("write new log");

        let mut follower = LogFollower::with_follow_start(path.clone(), snapshot.follow_start);
        assert_eq!(
            follower.poll().expect("poll created log"),
            vec![FollowEvent::Line("first\n".to_string())]
        );
    }

    #[tokio::test]
    async fn writer_handles_concurrent_emitters() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("events.jsonl");
        let mut writer = EventLogWriter::spawn(&path, true).await.expect("writer");
        let handle = writer.handle();

        let mut tasks = Vec::new();
        for index in 0..16 {
            let handle = handle.clone();
            tasks.push(tokio::spawn(async move {
                handle.emit("concurrent_event", json!({ "index": index }));
            }));
        }

        for task in tasks {
            task.await.expect("join");
        }

        drop(handle);
        writer.shutdown().await.expect("shutdown");

        let lines = read_tail_lines(&path, 32).expect("tail");
        assert_eq!(lines.len(), 16);

        for line in lines {
            let record: EventRecord = serde_json::from_str(&line).expect("json line");
            assert_eq!(record.event_type, "concurrent_event");
        }
    }

    #[tokio::test]
    async fn shutdown_flushes_queued_records_under_load() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("events.jsonl");
        let mut writer = EventLogWriter::spawn(&path, true).await.expect("writer");
        let handle = writer.handle();

        let task_count = 32usize;
        let events_per_task = 64usize;
        let total_events = task_count * events_per_task;
        let mut tasks = Vec::new();

        for task_id in 0..task_count {
            let handle = handle.clone();
            tasks.push(tokio::spawn(async move {
                for event_id in 0..events_per_task {
                    handle.emit(
                        "burst_event",
                        json!({
                            "task_id": task_id,
                            "event_id": event_id,
                        }),
                    );
                }
            }));
        }

        for task in tasks {
            task.await.expect("join");
        }

        drop(handle);
        writer.shutdown().await.expect("shutdown");

        let lines = read_tail_lines(&path, total_events + 1).expect("tail");
        assert_eq!(lines.len(), total_events);

        for line in lines {
            let record: EventRecord = serde_json::from_str(&line).expect("json line");
            assert_eq!(record.event_type, "burst_event");
        }
    }
}
