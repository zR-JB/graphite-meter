//! Layer state charged to the application's shared budget, the one noq charges its buffers to.
use noq::SharedBudget;
use std::sync::Arc;

pub type Budget = Option<Arc<dyn SharedBudget>>;

/// Bytes held against the budget until dropped; without a budget every charge succeeds.
pub(crate) struct Charge {
    budget: Budget,
    bytes: usize,
}

impl Charge {
    pub(crate) fn new(budget: &Budget, bytes: usize) -> Option<Self> {
        let mut charge = Self {
            budget: budget.clone(),
            bytes: 0,
        };
        charge.resize(bytes).then_some(charge)
    }

    /// Grows or shrinks the charge; growth the budget refuses leaves it unchanged.
    pub(crate) fn resize(&mut self, bytes: usize) -> bool {
        if let Some(budget) = &self.budget {
            if bytes > self.bytes && !budget.try_charge(bytes - self.bytes) {
                return false;
            }
            if bytes < self.bytes {
                budget.refund(self.bytes - bytes);
            }
        }
        self.bytes = bytes;
        true
    }
}

impl Drop for Charge {
    fn drop(&mut self) {
        self.resize(0);
    }
}
