//! USB topology capture for RP-series deploy diagnostics.
//!
//! Built entirely on [`fbuild_core::platform::device::available_serial_ports`]
//! facts: the host seam owns the SetupAPI/CfgMgr32 walk (Windows) and simply
//! reports no ancestry elsewhere. Two deliberate deltas versus the previous
//! hand-rolled fork: ports retained in host history (phantom records,
//! FastLED/fbuild#962) now get a topology line like live ones — better
//! failure diagnostics — and a port with neither an instance ID nor any
//! ancestor chain reports `None` instead of a flat "unavailable" sentence.

use fbuild_core::platform::device;

/// One-line human-readable USB topology for a runtime COM port, or `None`
/// when the host cannot supply one. Never panics: every missing fact
/// degrades to `None` rather than guessing at topology.
pub(super) fn describe_port_topology(port_name: &str) -> Option<String> {
    let facts = device::available_serial_ports().ok()?;
    let port = facts
        .iter()
        .find(|port| port.port_name.eq_ignore_ascii_case(port_name))?;
    if port.instance_id.is_none() && port.ancestor_instance_ids.is_empty() {
        // The host exposes no USB identity at all for this endpoint;
        // report nothing rather than a flat "unavailable" sentence.
        return None;
    }
    // An unreadable own ID degrades to no composite-ancestor skipping,
    // not to a lost topology line.
    let depth = classify_ancestor_chain(
        port.instance_id.as_deref().unwrap_or_default(),
        &port.ancestor_instance_ids,
    );
    Some(format_topology(depth, port.location_information.as_deref()))
}

/// Coarse USB hub-tier classification derived from an ancestor
/// instance-ID chain (child -> root order; the device's own ID is not
/// included). Pure string logic, deterministically testable on every
/// host OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HubDepth {
    DirectRootPort,
    BehindHubs(usize),
    /// The chain was empty, or didn't resolve to a recognizable
    /// hub/root-hub pattern before the ancestor walk gave up. Distinct
    /// from a genuine zero-hub result -- never guessed.
    Unavailable,
}

pub(super) fn classify_ancestor_chain(own_id: &str, ancestor_ids: &[String]) -> HubDepth {
    // A composite-device CDC function enumerates as an interface node
    // (`USB\VID_xxxx&PID_yyyy&MI_00\...`) whose leading ancestor is the USB
    // *device* node carrying the same VID&PID token — not a hub. Counting it
    // would over-report depth by one on every composite-CDC board, so drop
    // leading same-device ancestors before counting hub tiers.
    let own_token = vid_pid_token(own_id);
    let ancestor_ids = match &own_token {
        Some(token) => {
            let same_device = ancestor_ids
                .iter()
                .take_while(|id| id.to_ascii_uppercase().contains(token))
                .count();
            &ancestor_ids[same_device..]
        }
        None => ancestor_ids,
    };
    if ancestor_ids.is_empty() {
        return HubDepth::Unavailable;
    }
    if is_root_hub_id(&ancestor_ids[0]) {
        return HubDepth::DirectRootPort;
    }
    for (hub_tiers, id) in ancestor_ids.iter().enumerate() {
        if is_root_hub_id(id) {
            return HubDepth::BehindHubs(hub_tiers);
        }
        if !is_usb_device_id(id) {
            // Not a hub-shaped ancestor and not a root hub either: stop
            // rather than guess how many tiers remain.
            return HubDepth::Unavailable;
        }
    }
    // Walked off the end of the collected chain without reaching a root hub.
    HubDepth::Unavailable
}

/// Extract the uppercase `VID_xxxx&PID_yyyy` token from a USB instance ID,
/// dropping any interface suffix (`&MI_nn`). `None` for non-USB IDs.
fn vid_pid_token(instance_id: &str) -> Option<String> {
    let upper = instance_id.to_ascii_uppercase();
    let device_part = upper.strip_prefix("USB\\")?.split('\\').next()?;
    let mut fields = device_part.split('&');
    let vid = fields.next().filter(|field| field.starts_with("VID_"))?;
    let pid = fields.next().filter(|field| field.starts_with("PID_"))?;
    Some(format!("{vid}&{pid}"))
}

fn is_root_hub_id(instance_id: &str) -> bool {
    instance_id
        .to_ascii_uppercase()
        .starts_with("USB\\ROOT_HUB")
}

fn is_usb_device_id(instance_id: &str) -> bool {
    let upper = instance_id.to_ascii_uppercase();
    upper.starts_with("USB\\VID_") && upper.contains("&PID_")
}

/// Render a [`HubDepth`] plus an optional Windows location string
/// (`SPDRP_LOCATION_INFORMATION`, e.g. `Port_#0003.Hub_#0004`) into the
/// one-line summary appended to deploy failure messages. Facts fbuild
/// cannot query (hub power mode, sibling count) are labeled explicitly
/// rather than omitted or guessed.
pub(super) fn format_topology(depth: HubDepth, location: Option<&str>) -> String {
    match depth {
        HubDepth::DirectRootPort => match location {
            Some(location) => format!("USB topology: direct root port ({location})"),
            None => "USB topology: direct root port".to_string(),
        },
        HubDepth::BehindHubs(tiers) => {
            let at = match location {
                Some(location) => format!(" at {location}"),
                None => String::new(),
            };
            format!(
                "USB topology: behind {tiers} external hub tier(s){at}; hub power mode and sibling count unavailable (not queried)"
            )
        }
        HubDepth::Unavailable => "USB topology unavailable".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic non-composite own ID: no `&MI_nn` interface suffix, so its
    // VID&PID token never matches the hub ancestors below.
    const OWN_ID: &str = "USB\\VID_9999&PID_8888\\5303284720C4641C";

    #[test]
    fn direct_root_port_is_the_immediate_parent() {
        let ids = vec!["USB\\ROOT_HUB30\\4&1a2b3c4d&0".to_string()];
        assert_eq!(
            classify_ancestor_chain(OWN_ID, &ids),
            HubDepth::DirectRootPort
        );
    }

    #[test]
    fn single_external_hub_tier_is_counted() {
        let ids = vec![
            "USB\\VID_1111&PID_2222\\5&AABB".to_string(),
            "USB\\ROOT_HUB30\\4&1a2b3c4d&0".to_string(),
        ];
        assert_eq!(
            classify_ancestor_chain(OWN_ID, &ids),
            HubDepth::BehindHubs(1)
        );
    }

    #[test]
    fn hub_on_hub_counts_every_tier() {
        let ids = vec![
            "USB\\VID_1111&PID_2222\\5&AABB".to_string(),
            "USB\\VID_3333&PID_4444\\6&CCDD".to_string(),
            "USB\\VID_5555&PID_6666\\7&EEFF".to_string(),
            "USB\\ROOT_HUB30\\4&1a2b3c4d&0".to_string(),
        ];
        assert_eq!(
            classify_ancestor_chain(OWN_ID, &ids),
            HubDepth::BehindHubs(3)
        );
    }

    #[test]
    fn empty_or_unrecognizable_chain_is_unavailable() {
        assert_eq!(classify_ancestor_chain(OWN_ID, &[]), HubDepth::Unavailable);
        let garbage = vec!["PCI\\VEN_8086&DEV_1234\\3&11583659&0&D8".to_string()];
        assert_eq!(
            classify_ancestor_chain(OWN_ID, &garbage),
            HubDepth::Unavailable
        );
    }

    #[test]
    fn chain_that_never_reaches_a_root_hub_is_unavailable() {
        let ids = vec!["USB\\VID_1111&PID_2222\\5&AABB".to_string()];
        assert_eq!(classify_ancestor_chain(OWN_ID, &ids), HubDepth::Unavailable);
    }

    #[test]
    fn composite_device_node_is_not_counted_as_a_hub_tier() {
        let own_id = "USB\\VID_9999&PID_8888&MI_00\\7&99&0000";
        let ids = vec![
            "USB\\VID_9999&PID_8888\\5303284720C4641C".to_string(),
            "USB\\VID_1111&PID_2222\\5&AABB".to_string(),
            "USB\\ROOT_HUB30\\4&1a2b3c4d&0".to_string(),
        ];
        assert_eq!(
            classify_ancestor_chain(own_id, &ids),
            HubDepth::BehindHubs(1)
        );
    }

    #[test]
    fn composite_device_on_root_port_is_direct() {
        let own_id = "USB\\VID_9999&PID_8888&MI_00\\7&99&0000";
        let ids = vec![
            "USB\\VID_9999&PID_8888\\5303284720C4641C".to_string(),
            "USB\\ROOT_HUB30\\4&1a2b3c4d&0".to_string(),
        ];
        assert_eq!(
            classify_ancestor_chain(own_id, &ids),
            HubDepth::DirectRootPort
        );
    }

    #[test]
    fn own_id_without_vid_pid_token_skips_nothing() {
        let ids = vec![
            "USB\\VID_1111&PID_2222\\5&AABB".to_string(),
            "USB\\ROOT_HUB30\\4&1a2b3c4d&0".to_string(),
        ];
        assert_eq!(
            classify_ancestor_chain("FTDIBUS\\COMPORT&VID_0403", &ids),
            HubDepth::BehindHubs(1)
        );
        assert_eq!(classify_ancestor_chain("", &ids), HubDepth::BehindHubs(1));
    }

    #[test]
    fn direct_root_port_formats_with_and_without_location() {
        assert_eq!(
            format_topology(HubDepth::DirectRootPort, Some("Port_#0001.Hub_#0002")),
            "USB topology: direct root port (Port_#0001.Hub_#0002)"
        );
        assert_eq!(
            format_topology(HubDepth::DirectRootPort, None),
            "USB topology: direct root port"
        );
    }

    #[test]
    fn behind_hubs_labels_unqueried_facts_explicitly() {
        assert_eq!(
            format_topology(HubDepth::BehindHubs(2), Some("Port_#0003.Hub_#0004")),
            "USB topology: behind 2 external hub tier(s) at Port_#0003.Hub_#0004; hub power mode and sibling count unavailable (not queried)"
        );
        assert_eq!(
            format_topology(HubDepth::BehindHubs(1), None),
            "USB topology: behind 1 external hub tier(s); hub power mode and sibling count unavailable (not queried)"
        );
    }

    #[test]
    fn unavailable_chain_yields_the_flat_fallback_message() {
        assert_eq!(
            format_topology(HubDepth::Unavailable, Some("Port_#0001.Hub_#0002")),
            "USB topology unavailable"
        );
    }

    #[test]
    fn unknown_port_reports_no_topology_on_any_host() {
        // Host-independent contract: a port name the host never enumerated
        // yields `None` everywhere (Windows included), never a guess.
        assert_eq!(describe_port_topology("FBUILD_NO_SUCH_PORT"), None);
    }
}
