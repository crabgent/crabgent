/// Implement the `ChannelInbox` pass-through methods for a decorator.
#[macro_export]
macro_rules! forward_channel_inbox_methods {
    ($inner:ident) => {
        fn receive_reaction<'life0, 'async_trait>(
            &'life0 self,
            reaction: $crate::InboundReaction,
        ) -> ::std::pin::Pin<
            Box<
                dyn ::std::future::Future<Output = Result<(), $crate::ChannelError>>
                    + Send
                    + 'async_trait,
            >,
        >
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move { self.$inner.receive_reaction(reaction).await })
        }

        fn shutdown<'life0, 'async_trait>(
            &'life0 self,
            grace: std::time::Duration,
        ) -> ::std::pin::Pin<Box<dyn ::std::future::Future<Output = ()> + Send + 'async_trait>>
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move {
                self.$inner.shutdown(grace).await;
            })
        }

        fn blocks_outer_command_dispatch(&self) -> bool {
            self.$inner.blocks_outer_command_dispatch()
        }
    };
}
