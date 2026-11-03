//! Opt-in bounded wait for a foreign exclusive device lease (FastLED/fbuild#1864).
//!
//! A competing workflow that must not fail fast can join a per-port FIFO queue
//! and wait for the current holder to release the board. Ordering is
//! deterministic: a ticket is admitted only when it is at the *front* of the
//! port's queue, so two waiters that observe a release in the same instant can
//! never reorder. A [`QueueGuard`] owns the ticket for the whole async wait, so
//! a cancelled request (dropped future) removes itself and promotes the next
//! workflow instead of leaving a phantom head-of-line blocker.
//!
//! Waiting always happens *before* preemption/reset/open: callers admit first
//! and only then touch the port, so a queued workflow never disturbs the
//! holder's board.

use super::{DeviceLease, DeviceLeaseError, DeviceManager};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// One queued workflow, ordered by `seq` (FIFO within a port).
#[derive(Debug, Clone)]
pub(crate) struct LeaseWaiter {
    pub(crate) seq: u64,
    pub(crate) client_id: String,
}

/// A workflow's place in a port's admission queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueTicket {
    seq: u64,
}

impl QueueTicket {
    /// Monotonic sequence number assigned at enqueue time.
    pub fn sequence(&self) -> u64 {
        self.seq
    }
}

/// Outcome of one admission poll.
#[derive(Debug, Clone)]
pub enum AdmissionPoll {
    /// The caller may proceed: no foreign lease, and nothing is queued ahead.
    Admitted,
    /// Another workflow is ahead, or a foreign lease still reserves the port.
    Waiting {
        /// 1-based place in the admission queue.
        position: usize,
        /// Current exclusive-lease holder, when one reserves the port.
        holder: Option<Box<DeviceLease>>,
    },
}

impl AdmissionPoll {
    pub fn is_admitted(&self) -> bool {
        matches!(self, Self::Admitted)
    }
}

/// Holds a queue ticket; dropping it leaves the queue.
///
/// The guard deliberately does **not** remember the port: a tracked
/// re-enumeration can rename the device while the workflow is queued, and a
/// ticket is unique across the manager, so leaving by ticket is the only
/// removal that cannot strand a phantom head-of-line waiter on the old name.
pub struct QueueGuard {
    manager: Arc<DeviceManager>,
    ticket: QueueTicket,
}

impl QueueGuard {
    pub fn ticket(&self) -> QueueTicket {
        self.ticket
    }
}

impl Drop for QueueGuard {
    fn drop(&mut self) {
        self.manager.leave_ticket(self.ticket);
    }
}

impl DeviceManager {
    /// Join the admission queue for `port`. The ticket stays live until the
    /// returned sequence is removed by [`Self::leave_queue`] (a guard does it
    /// on drop).
    pub fn enqueue_waiter(&self, port: &str, client_id: &str) -> QueueTicket {
        let seq = self
            .next_waiter_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let ticket = QueueTicket { seq };
        {
            let mut waiters = self.waiters.lock().unwrap_or_else(|e| e.into_inner());
            waiters
                .entry(port.to_string())
                .or_default()
                .push_back(LeaseWaiter {
                    seq,
                    client_id: client_id.to_string(),
                });
        }
        tracing::info!(
            "workflow '{}' queued for device '{}' (ticket {})",
            client_id,
            port,
            seq
        );
        ticket
    }

    /// Remove `ticket` from the port's queue (no-op when already gone).
    pub fn leave_queue(&self, port: &str, ticket: QueueTicket) {
        let mut waiters = self.waiters.lock().unwrap_or_else(|e| e.into_inner());
        let Some(queue) = waiters.get_mut(port) else {
            return;
        };
        let before = queue.len();
        queue.retain(|waiter| waiter.seq != ticket.seq);
        if queue.len() != before {
            tracing::info!(
                "workflow left the queue for device '{}' (ticket {})",
                port,
                ticket.sequence()
            );
        }
        if queue.is_empty() {
            waiters.remove(port);
        }
    }

    /// Remove `ticket` from whichever queue holds it (no-op when already gone).
    ///
    /// Used by [`QueueGuard::drop`], which cannot rely on the port name
    /// surviving a re-enumeration (FastLED/fbuild#1864 criterion 4).
    pub fn leave_ticket(&self, ticket: QueueTicket) {
        let mut waiters = self.waiters.lock().unwrap_or_else(|e| e.into_inner());
        let stale: Vec<String> = waiters
            .iter_mut()
            .filter_map(|(port, queue)| {
                queue.retain(|waiter| waiter.seq != ticket.seq);
                queue.is_empty().then(|| port.clone())
            })
            .collect();
        for port in stale {
            waiters.remove(&port);
        }
    }

    /// Move a port's admission queue to the port a tracked board
    /// re-enumerated as, preserving FIFO order across the move.
    ///
    /// Returns the number of workflows moved. Called from
    /// [`DeviceManager::refresh_devices`] (queue follows the migrated
    /// `DeviceState`) and while waiting (a renumber mid-wait).
    pub(crate) fn migrate_waiters(&self, from: &str, to: &str) -> usize {
        if from == to {
            return 0;
        }
        let mut waiters = self.waiters.lock().unwrap_or_else(|e| e.into_inner());
        let Some(mut moving) = waiters.remove(from) else {
            return 0;
        };
        let moved = moving.len();
        match waiters.get_mut(to) {
            Some(queue) => {
                queue.append(&mut moving);
                // Ticket sequences are unique and monotonic, so sorting keeps
                // the original arrival order across both queues (`VecDeque`
                // has no in-place `sort_by_key`).
                let mut merged: Vec<LeaseWaiter> = queue.drain(..).collect();
                merged.sort_by_key(|waiter| waiter.seq);
                queue.extend(merged);
            }
            None => {
                waiters.insert(to.to_string(), moving);
            }
        }
        tracing::info!(
            "moved {} queued workflow(s) from '{}' to '{}' after re-enumeration",
            moved,
            from,
            to
        );
        moved
    }

    /// Follow a tracked re-enumeration from a stale port name to the port the
    /// board uses now. Bounded so a corrupt `previous_port` chain cannot spin.
    pub(crate) fn current_port_for(&self, port: &str) -> String {
        let mut current = port.to_string();
        for _ in 0..8 {
            let next = {
                let devices = self.devices.lock().unwrap_or_else(|e| e.into_inner());
                super::renumbered_target(&devices, &current).map(|state| state.port.clone())
            };
            match next {
                Some(next) => current = next,
                None => break,
            }
        }
        current
    }

    /// Follow a tracked re-enumeration mid-wait: move this port's queue to the
    /// name the board uses now and return the port to poll.
    fn follow_renumbered(&self, port: &str, client_id: &str) -> String {
        let resolved = self.current_port_for(port);
        if resolved == port {
            return resolved;
        }
        tracing::info!(
            "workflow '{}' follows device '{}' -> '{}' after re-enumeration",
            client_id,
            port,
            resolved
        );
        self.migrate_waiters(port, &resolved);
        resolved
    }

    /// Client ids waiting on `port`, in queue order (diagnostics).
    pub fn queued_client_ids(&self, port: &str) -> Vec<String> {
        let waiters = self.waiters.lock().unwrap_or_else(|e| e.into_inner());
        waiters
            .get(port)
            .map(|queue| queue.iter().map(|w| w.client_id.clone()).collect())
            .unwrap_or_default()
    }

    /// Number of workflows waiting on `port` (diagnostics / status output).
    pub fn queued_workflows(&self, port: &str) -> usize {
        let waiters = self.waiters.lock().unwrap_or_else(|e| e.into_inner());
        waiters.get(port).map(|queue| queue.len()).unwrap_or(0)
    }

    /// One admission poll. `ticket` is `None` for a fail-fast caller that has
    /// not joined the queue; such a caller is admitted only when the port is
    /// free *and* nobody is already queued (so a newcomer cannot jump a queue),
    /// and a `Waiting.position` of `len + 1` reports where it would land.
    ///
    /// Lock order is always `devices` then `waiters`.
    pub fn poll_admission(
        &self,
        port: &str,
        lease_id: Option<&str>,
        ticket: Option<QueueTicket>,
    ) -> AdmissionPoll {
        let (holder, is_holder) = {
            let devices = self.devices.lock().unwrap_or_else(|e| e.into_inner());
            let Some(state) = devices.get(port) else {
                // Unknown ports (emulator/probe routes) stay outside the lease
                // path, matching `admit_operation` (FastLED/fbuild#1864).
                return AdmissionPoll::Admitted;
            };
            let holder = state.exclusive_lease.clone();
            let is_holder = holder
                .as_ref()
                .is_some_and(|held| Some(held.lease_id.as_str()) == lease_id);
            (holder, is_holder)
        };
        if is_holder {
            // The lease holder always goes first, queued or not.
            return AdmissionPoll::Admitted;
        }
        let ahead = {
            let waiters = self.waiters.lock().unwrap_or_else(|e| e.into_inner());
            match waiters.get(port) {
                None => 0,
                Some(queue) => match ticket {
                    Some(ticket) => queue.iter().take_while(|w| w.seq < ticket.seq).count(),
                    None => queue.len(),
                },
            }
        };
        if ahead == 0 && holder.is_none() {
            AdmissionPoll::Admitted
        } else {
            AdmissionPoll::Waiting {
                position: ahead + 1,
                holder: holder.map(Box::new),
            }
        }
    }

    /// Opt-in bounded wait for admission on `port`.
    ///
    /// Returns the milliseconds spent waiting (0 when admitted immediately).
    /// `wait_ms == 0` keeps the fail-fast behaviour: the caller is refused with
    /// [`DeviceLeaseError::LeaseWaitTimedOut`] describing the holder and the
    /// queue depth. A non-zero `wait_ms` joins the FIFO queue and either is
    /// admitted by the deadline or times out without ever touching the port.
    pub async fn await_admission(
        self: &Arc<Self>,
        port: &str,
        lease_id: Option<&str>,
        wait_ms: u64,
        client_id: &str,
    ) -> Result<u64, DeviceLeaseError> {
        let started = Instant::now();
        // A tracked USB renumber renames the device under the caller; polling
        // the old name would report "unknown port" and admit the workflow
        // while the lease sits on the new one (FastLED/fbuild#1864 criterion 4).
        let mut current = self.follow_renumbered(port, client_id);
        if self.poll_admission(&current, lease_id, None).is_admitted() {
            return Ok(0);
        }
        if wait_ms == 0 {
            let AdmissionPoll::Waiting { position, holder } =
                self.poll_admission(&current, lease_id, None)
            else {
                return Ok(0);
            };
            return Err(DeviceLeaseError::LeaseWaitTimedOut {
                port: current,
                waited_ms: 0,
                position,
                holder,
            });
        }

        let guard = QueueGuard {
            manager: Arc::clone(self),
            ticket: self.enqueue_waiter(&current, client_id),
        };
        let ticket = guard.ticket();
        let deadline = started + Duration::from_millis(wait_ms);
        let mut backoff = Duration::from_millis(20);
        loop {
            current = self.follow_renumbered(&current, client_id);
            match self.poll_admission(&current, lease_id, Some(ticket)) {
                AdmissionPoll::Admitted => {
                    let waited_ms = started.elapsed().as_millis() as u64;
                    if waited_ms > 0 {
                        tracing::info!(
                            "workflow '{}' admitted on device '{}' after waiting {} ms",
                            client_id,
                            current,
                            waited_ms
                        );
                    }
                    drop(guard);
                    return Ok(waited_ms);
                }
                AdmissionPoll::Waiting { position, holder } => {
                    let now = Instant::now();
                    if now >= deadline {
                        tracing::warn!(
                            "workflow '{}' gave up waiting for device '{}': position {}, queued behind {:?}",
                            client_id,
                            current,
                            position,
                            self.queued_client_ids(&current)
                        );
                        return Err(DeviceLeaseError::LeaseWaitTimedOut {
                            port: current.clone(),
                            waited_ms: started.elapsed().as_millis() as u64,
                            position,
                            holder,
                        });
                    }
                    tokio::time::sleep(backoff.min(deadline - now)).await;
                    backoff = (backoff * 2).min(Duration::from_millis(200));
                }
            }
        }
    }
}
