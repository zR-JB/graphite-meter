#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureReason {
    PreparationFailed,
    ConnectionLost,
    Timeout,
    SignInRequired,
    ServerBusy,
    ProtocolError,
    InsufficientEvidence,
}

impl FailureReason {
    fn row(self) -> (&'static str, &'static str) {
        include_str!("../../../api/failurereasons.txt")
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .nth(self as usize)
            .and_then(|line| line.split_once('|'))
            .map(|(name, label)| (name.trim(), label.trim()))
            .expect("failure vocabulary is pinned")
    }
    pub fn name(self) -> &'static str {
        self.row().0
    }
    pub fn label(self) -> &'static str {
        self.row().1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneEnding {
    Finished,
    Idle,
    Lifetime,
    Revoked,
    Shutdown,
}

impl LaneEnding {
    fn row(self) -> [&'static str; 4] {
        let mut fields = include_str!("../../../api/laneendings.txt")
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .nth(self as usize)
            .expect("lane endings are pinned")
            .split('|')
            .map(str::trim);
        [
            fields.next().unwrap(),
            fields.next().unwrap(),
            fields.next().unwrap(),
            fields.next().unwrap(),
        ]
    }
    pub fn name(self) -> &'static str {
        self.row()[0]
    }
    pub fn websocket_code(self) -> u16 {
        self.row()[1].parse().expect("pinned WebSocket code")
    }
    pub fn webtransport_code(self) -> u32 {
        self.row()[2].parse().expect("pinned WebTransport code")
    }
    pub fn reason(self) -> &'static str {
        self.row()[3]
    }
    pub fn from_websocket_code(code: u16) -> Option<Self> {
        [
            Self::Finished,
            Self::Idle,
            Self::Lifetime,
            Self::Revoked,
            Self::Shutdown,
        ]
        .into_iter()
        .find(|ending| ending.websocket_code() == code)
    }
    pub fn from_webtransport_code(code: u32) -> Option<Self> {
        [
            Self::Finished,
            Self::Idle,
            Self::Lifetime,
            Self::Revoked,
            Self::Shutdown,
        ]
        .into_iter()
        .find(|ending| ending.webtransport_code() == code)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadRefusal {
    Invalid,
    GlobalFull,
    ClientFull,
    OwnerMismatch,
    Idle,
    Revoked,
}

impl UploadRefusal {
    fn row(self) -> [&'static str; 3] {
        let mut fields = include_str!("../../../api/uploadrefusals.txt")
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .nth(self as usize)
            .expect("upload refusals are pinned")
            .split('|')
            .map(str::trim);
        [
            fields.next().unwrap(),
            fields.next().unwrap(),
            fields.next().unwrap(),
        ]
    }
    pub fn name(self) -> &'static str {
        self.row()[0]
    }
    pub fn message(self) -> &'static str {
        self.row()[1]
    }
    pub fn status(self) -> u16 {
        self.row()[2].parse().expect("pinned refusal status")
    }
    pub fn from_name(name: &str) -> Option<Self> {
        [
            Self::Invalid,
            Self::GlobalFull,
            Self::ClientFull,
            Self::OwnerMismatch,
            Self::Idle,
            Self::Revoked,
        ]
        .into_iter()
        .find(|refusal| refusal.name() == name)
    }
}
