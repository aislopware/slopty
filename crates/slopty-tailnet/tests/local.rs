//! Against this machine's own Tailscale, when one is running and readable: the node finds
//! itself in its status, and the daemon names its own user as the caller from its own address.
//! A machine without Tailscale skips, saying so.

#[cfg(test)]
mod tests {
    use slopty_tailnet::{Grant, LocalApi};
    #[tokio::test]
    async fn this_machine_is_its_own_users_node() {
        let Some(api) = LocalApi::find() else {
            slopty_testkit::live::skip("no Tailscale LocalAPI this user can read");
            return;
        };
        let status = api.status().await.expect("the daemon answers");
        if !status.running() {
            slopty_testkit::live::skip(&format!("tailscale is {}", status.backend_state));
            return;
        }
        let me = status.me.as_ref().expect("a running node has itself");
        let ip = me.ipv4().expect("every node has an IPv4 address");
        assert!(status.node_at(ip).is_some_and(|n| n.id == me.id));
        let who = api
            .whois((ip, 1).into())
            .await
            .expect("the daemon answers")
            .expect("the daemon knows its own address");
        assert_eq!(who.node.name, me.dns_name);
        let owner = me.tags.is_empty().then_some(me.user);
        if owner.is_some() {
            assert_eq!(Grant::of(&who, owner), Grant::ALL, "the user's own machine");
        }
    }
}
