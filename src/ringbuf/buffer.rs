use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

pub const DEFAULT_CAPACITY: usize = 10_000;
pub const DEFAULT_MAX_BYTES: usize = 2 * 1024 * 1024; // 2MB

/// Classification of output streams
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LogStream {
    Stdout,
    Stderr,
    System,
}

pub type StreamType = LogStream;

impl std::fmt::Display for LogStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogStream::Stdout => write!(f, "stdout"),
            LogStream::Stderr => write!(f, "stderr"),
            LogStream::System => write!(f, "system"),
        }
    }
}

/// A structured log line emitted by a supervised service or system event
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    /// Monotonically increasing sequence number
    pub sequence: u64,
    /// UTC timestamp of log reception
    pub timestamp: DateTime<Utc>,
    /// Stream origin
    pub stream: LogStream,
    /// Name or identifier of emitting service
    pub service_name: String,
    /// Raw line message content (preserving ANSI color codes)
    pub message: String,
    /// Byte length of the raw message
    pub byte_len: usize,
}

impl LogEntry {
    pub fn new(
        service_name: impl Into<String>,
        stream: LogStream,
        message: impl Into<String>,
    ) -> Self {
        let service_name = service_name.into();
        let message = message.into();
        let byte_len = message.len();
        Self {
            sequence: 0,
            timestamp: Utc::now(),
            stream,
            service_name,
            message,
            byte_len,
        }
    }

    pub fn with_sequence(mut self, sequence: u64) -> Self {
        self.sequence = sequence;
        self
    }

    pub fn with_timestamp(mut self, timestamp: DateTime<Utc>) -> Self {
        self.timestamp = timestamp;
        self
    }

    /// Returns the message with all ANSI escape sequences stripped
    pub fn plain_text(&self) -> String {
        strip_ansi(&self.message)
    }

    /// Formats entry as: `[HH:MM:SS.mmm] [stdout] message`
    pub fn formatted(&self) -> String {
        format!(
            "[{}] [{}] {}",
            self.timestamp.format("%H:%M:%S%.3f"),
            self.stream,
            self.message
        )
    }

    /// Formats entry including service name: `[HH:MM:SS.mmm] [service] [stdout] message`
    pub fn formatted_with_service(&self) -> String {
        format!(
            "[{}] [{}] [{}] {}",
            self.timestamp.format("%H:%M:%S%.3f"),
            self.service_name,
            self.stream,
            self.message
        )
    }
}

/// Strips standard ANSI terminal escape sequences (colors, styles, cursor control)
pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if let Some(&next) = chars.peek() {
                if next == '[' {
                    // CSI sequence: ESC [ ... FinalByte in @..~
                    chars.next();
                    for code in chars.by_ref() {
                        if ('@'..='~').contains(&code) {
                            break;
                        }
                    }
                } else if next == ']' {
                    // OSC sequence: ESC ] ... BEL (\x07) or ST (\x1b\)
                    chars.next();
                    while let Some(osc_c) = chars.next() {
                        if osc_c == '\x07' {
                            break;
                        }
                        if osc_c == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                } else {
                    // Other two-character escape sequence
                    chars.next();
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Trait abstraction for log ring buffers
pub trait LogBuffer: Send + Sync {
    fn push(&self, entry: LogEntry);
    fn snapshot(&self, limit: usize) -> Vec<LogEntry>;
    fn clear(&self);
}

/// High-throughput, circular lock-free in-memory ring-buffer
pub struct RingBuffer {
    capacity: usize,
    max_bytes: usize,
    head: AtomicU64,
    dropped_entries: AtomicU64,
    total_bytes: AtomicUsize,
    slots: Vec<arc_swap::ArcSwapOption<LogEntry>>,
}

pub type CircularLogBuffer = RingBuffer;

impl Default for RingBuffer {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY, DEFAULT_MAX_BYTES)
    }
}

impl RingBuffer {
    /// Creates a new circular ring buffer with the given line capacity and byte quota
    pub fn new(capacity: usize, max_bytes: usize) -> Self {
        let cap = capacity.max(1);
        let mut slots = Vec::with_capacity(cap);
        for _ in 0..cap {
            slots.push(arc_swap::ArcSwapOption::new(None));
        }

        Self {
            capacity: cap,
            max_bytes,
            head: AtomicU64::new(0),
            dropped_entries: AtomicU64::new(0),
            total_bytes: AtomicUsize::new(0),
            slots,
        }
    }

    /// Pushes an entry into the buffer atomically without locking
    pub fn push(&self, mut entry: LogEntry) {
        let entry_bytes = entry.byte_len;
        let seq = self.head.fetch_add(1, Ordering::SeqCst);
        entry.sequence = seq;

        let entry_arc = Arc::new(entry);
        let slot_idx = (seq as usize) % self.capacity;

        let old = self.slots[slot_idx].swap(Some(entry_arc));
        if let Some(old_entry) = old {
            self.dropped_entries.fetch_add(1, Ordering::Relaxed);
            self.total_bytes
                .fetch_sub(old_entry.byte_len, Ordering::Relaxed);
        }
        self.total_bytes.fetch_add(entry_bytes, Ordering::Relaxed);
    }

    /// Returns a snapshot of the most recent entries up to `limit` in chronological order
    pub fn snapshot(&self, limit: usize) -> Vec<LogEntry> {
        if limit == 0 {
            return Vec::new();
        }

        let head = self.head.load(Ordering::SeqCst);
        if head == 0 {
            return Vec::new();
        }

        let available = head.min(self.capacity as u64);
        let fetch_count = (limit as u64).min(available) as usize;
        let start_seq = head.saturating_sub(fetch_count as u64);

        let mut entries = Vec::with_capacity(fetch_count);
        for seq in start_seq..head {
            let slot_idx = (seq as usize) % self.capacity;
            if let Some(entry) = self.slots[slot_idx].load_full() {
                if entry.sequence == seq {
                    entries.push((*entry).clone());
                }
            }
        }
        entries
    }

    /// Returns wait-free snapshot of `Arc<LogEntry>` without copying message payloads
    pub fn read_recent(&self, limit: usize) -> Vec<Arc<LogEntry>> {
        if limit == 0 {
            return Vec::new();
        }

        let head = self.head.load(Ordering::SeqCst);
        if head == 0 {
            return Vec::new();
        }

        let available = head.min(self.capacity as u64);
        let fetch_count = (limit as u64).min(available) as usize;
        let start_seq = head.saturating_sub(fetch_count as u64);

        let mut entries = Vec::with_capacity(fetch_count);
        for seq in start_seq..head {
            let slot_idx = (seq as usize) % self.capacity;
            if let Some(entry) = self.slots[slot_idx].load_full() {
                if entry.sequence == seq {
                    entries.push(entry);
                }
            }
        }
        entries
    }

    /// Returns the current monotonic head sequence counter
    pub fn head_sequence(&self) -> u64 {
        self.head.load(Ordering::Relaxed)
    }

    /// Returns current number of retained entries in the buffer
    pub fn len(&self) -> usize {
        let head = self.head.load(Ordering::Relaxed);
        head.min(self.capacity as u64) as usize
    }

    /// Returns true if buffer contains no log entries
    pub fn is_empty(&self) -> bool {
        self.head.load(Ordering::Relaxed) == 0
    }

    /// Returns total number of overwritten / dropped log entries
    pub fn dropped_count(&self) -> u64 {
        self.dropped_entries.load(Ordering::Relaxed)
    }

    /// Returns current cumulative byte size of stored messages
    pub fn total_bytes(&self) -> usize {
        self.total_bytes.load(Ordering::Relaxed)
    }

    /// Returns maximum line capacity
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns maximum configured byte quota
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Clears all entries from the buffer
    pub fn clear(&self) {
        for slot in &self.slots {
            slot.store(None);
        }
        self.head.store(0, Ordering::SeqCst);
        self.dropped_entries.store(0, Ordering::SeqCst);
        self.total_bytes.store(0, Ordering::SeqCst);
    }
}

impl LogBuffer for RingBuffer {
    fn push(&self, entry: LogEntry) {
        self.push(entry);
    }

    fn snapshot(&self, limit: usize) -> Vec<LogEntry> {
        self.snapshot(limit)
    }

    fn clear(&self) {
        self.clear();
    }
}

/// Spawns an asynchronous background task to stream lines from an async reader into the buffer
pub fn spawn_stream_reader<R>(
    reader: R,
    service_name: String,
    stream: LogStream,
    buffer: Arc<RingBuffer>,
) -> tokio::task::JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            buffer.push(LogEntry::new(service_name.clone(), stream, line));
        }
    })
}
