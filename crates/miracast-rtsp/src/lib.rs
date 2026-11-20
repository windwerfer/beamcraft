#![forbid(unsafe_code)]
//! Miracast sink-side RTSP (messages M1-M7, keepalive, teardown), sans-IO.
//!
//! Skeleton (step 3). The state machine and the `wfd_*` parameter parsers arrive in step 4
//! (`docs/agents/miracast.md` section 3). The shape: the caller feeds bytes read from the
//! TCP connection, the machine returns [`Action`]s (bytes to send, media to start/stop).

/// Where the sink is in the M1-M7 sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum State {
    /// Waiting for the source's M1 (`OPTIONS`).
    #[default]
    AwaitOptions,
    /// M1 answered, M2 sent; waiting for M3 (`GET_PARAMETER`).
    AwaitCapabilityQuery,
    /// M3 answered; waiting for M4 (`SET_PARAMETER` with the chosen formats).
    AwaitParameters,
    /// M4 accepted; waiting for M5 (`wfd_trigger_method: SETUP`).
    AwaitTrigger,
    /// M6 (`SETUP`) sent; waiting for the reply.
    AwaitSetupReply,
    /// M7 (`PLAY`) sent; waiting for the reply.
    AwaitPlayReply,
    /// Media flows; keepalives (M16) answered.
    Playing,
    /// TEARDOWN seen or sent, or a fatal error.
    Closed,
}

/// What the I/O layer must do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Write these bytes to the RTSP TCP connection.
    Send(Vec<u8>),
    /// Start receiving RTP on the announced port.
    StartMedia {
        rtp_port: u16,
    },
    StopMedia,
    /// Close the connection; the reason is shown in the GUI (AGENTS.md 1.8).
    Close {
        reason: String,
    },
}

/// Sink-side session (stub).
#[derive(Debug, Default)]
pub struct SinkSession {
    state: State,
}

impl SinkSession {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> State {
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_waiting_for_m1() {
        assert_eq!(SinkSession::new().state(), State::AwaitOptions);
    }
}
