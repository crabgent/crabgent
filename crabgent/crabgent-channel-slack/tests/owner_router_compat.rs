use std::sync::Arc;

use async_trait::async_trait;
use crabgent_channel::{
    Channel, ChannelError, ChannelKind, ChannelRouter, ChannelSink, MessageRef, OutboundMessage,
    Participant,
};
use crabgent_core::owner::Owner;
use crabgent_core::subject::Subject;

struct StubSlackChannel;

#[async_trait]
impl Channel for StubSlackChannel {
    fn name(&self) -> &'static str {
        "slack"
    }

    async fn kind(&self, _conv: &Owner) -> Result<ChannelKind, ChannelError> {
        Ok(ChannelKind::Group)
    }

    async fn participants(
        &self,
        _ctx: &Subject,
        _conv: &Owner,
    ) -> Result<Vec<Participant>, ChannelError> {
        Ok(Vec::new())
    }

    async fn send(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        _msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        Ok(MessageRef::top_level(
            "slack",
            conv.clone(),
            "1710000000.000100",
        ))
    }
}

#[tokio::test]
async fn channel_router_routes_slack_owner_prefix() {
    let router = ChannelRouter::new().with_channel(Arc::new(StubSlackChannel));
    let message = OutboundMessage::new("hello");
    let response = router
        .send(&Subject::new("agent"), &Owner::new("slack:T1/C1"), &message)
        .await
        .expect("slack route should resolve");

    assert_eq!(response.channel, "slack");
    assert_eq!(response.conv.as_str(), "slack:T1/C1");
}
