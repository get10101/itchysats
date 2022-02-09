use anyhow::Result;
use daemon::command;
use daemon::model::cfd::OrderId;
use daemon::monitor;
use daemon::oracle;
use std::sync::Arc;
use tokio::sync::Mutex;
use xtra_productivity::xtra_productivity;

/// Test Stub simulating the Monitor actor.
/// Serves as an entrypoint for injected mock handlers.
pub struct MonitorActor {
    _mock: Arc<Mutex<MockMonitor>>,
}

impl MonitorActor {
    pub fn new(executor: command::Executor) -> (Self, Arc<Mutex<MockMonitor>>) {
        let mock = Arc::new(Mutex::new(MockMonitor::new(executor)));
        let actor = Self {
            _mock: mock.clone(),
        };

        (actor, mock)
    }
}

impl xtra::Actor for MonitorActor {}

#[xtra_productivity(message_impl = false)]
impl MonitorActor {
    async fn handle(&mut self, _: monitor::Sync) {}

    async fn handle(&mut self, _: monitor::StartMonitoring) {}

    async fn handle(&mut self, _: monitor::CollaborativeSettlement) {}

    async fn handle(&mut self, _: oracle::Attestation) {}

    async fn handle(&mut self, _: monitor::TryBroadcastTransaction) -> Result<()> {
        Ok(())
    }
}

pub struct MockMonitor {
    executor: command::Executor,
}

impl MockMonitor {
    pub fn new(executor: command::Executor) -> Self {
        MockMonitor { executor }
    }

    pub async fn confirm_lock_transaction(&mut self, id: OrderId) {
        self.executor
            .execute(id, |cfd| Ok(cfd.handle_lock_confirmed()))
            .await
            .unwrap();
    }

    pub async fn confirm_commit_transaction(&mut self, id: OrderId) {
        self.executor
            .execute(id, |cfd| Ok(cfd.handle_commit_confirmed()))
            .await
            .unwrap();
    }

    pub async fn expire_refund_timelock(&mut self, id: OrderId) {
        self.executor
            .execute(id, |cfd| cfd.handle_refund_timelock_expired())
            .await
            .unwrap();
    }

    pub async fn confirm_refund_transaction(&mut self, id: OrderId) {
        self.executor
            .execute(id, |cfd| Ok(cfd.handle_refund_confirmed()))
            .await
            .unwrap();
    }

    pub async fn expire_cet_timelock(&mut self, id: OrderId) {
        self.executor
            .execute(id, |cfd| cfd.handle_cet_timelock_expired())
            .await
            .unwrap();
    }

    pub async fn confirm_cet(&mut self, id: OrderId) {
        self.executor
            .execute(id, |cfd| Ok(cfd.handle_cet_confirmed()))
            .await
            .unwrap();
    }

    pub async fn confirm_close_transaction(&mut self, id: OrderId) {
        self.executor
            .execute(
                id,
                |cfd| Ok(cfd.handle_collaborative_settlement_confirmed()),
            )
            .await
            .unwrap();
    }
}
