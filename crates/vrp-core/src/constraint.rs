//! 约束标识和权重。名称、默认权重与 docs/constraints-v2.md 第 2 节一一对应。

/// 一条约束的权重：匹配量 × 权重计入对应级别。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Weight {
    pub hard: i64,
    pub medium: i64,
    pub soft: i64,
}

impl Weight {
    pub const ZERO: Weight = Weight {
        hard: 0,
        medium: 0,
        soft: 0,
    };
    /// 每个级别允许的最大权重；完整计分上界由 Problem 编译时另行验证。
    pub const MAX_COMPONENT: i64 = 1_000_000_000;

    pub const fn hard(value: i64) -> Weight {
        Weight {
            hard: value,
            medium: 0,
            soft: 0,
        }
    }

    pub const fn medium(value: i64) -> Weight {
        Weight {
            hard: 0,
            medium: value,
            soft: 0,
        }
    }

    pub const fn soft(value: i64) -> Weight {
        Weight {
            hard: 0,
            medium: 0,
            soft: value,
        }
    }

    pub fn is_zero(self) -> bool {
        self == Self::ZERO
    }
}

pub const CONSTRAINT_COUNT: usize = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ConstraintId {
    AgentCapacity,
    AgentMaxTicket,
    AgentSkills,
    AgentQualification,
    RefTicketAfterDepTicket,
    RefTicketSameAgentWithDepTicket,
    ServiceFinishedAfterMaxEndTime,
    TicketStartServiceTimeMatchExpected,
    TicketArrivalTimeSameDateWithPlanTime,
    RelationTicketsSameAgent,
    MinimizeTravelTime,
    MinimizeTravelDistance,
    MinimizeAgentFixedCost,
    SameDepo,
    BalanceAgentLoading,
    BalanceAgentLoadingRatio,
    BalanceAgentWorkingTime,
    MinimizeTicketChanging,
    TicketUnassigned,
    UnreachableLeg,
    AgentShiftOvertime,
    ExecutionConsistency,
    MinimizeVisitOrderChanging,
    MinimizeServiceTimeChanging,
}

impl ConstraintId {
    pub const ALL: [ConstraintId; CONSTRAINT_COUNT] = [
        ConstraintId::AgentCapacity,
        ConstraintId::AgentMaxTicket,
        ConstraintId::AgentSkills,
        ConstraintId::AgentQualification,
        ConstraintId::RefTicketAfterDepTicket,
        ConstraintId::RefTicketSameAgentWithDepTicket,
        ConstraintId::ServiceFinishedAfterMaxEndTime,
        ConstraintId::TicketStartServiceTimeMatchExpected,
        ConstraintId::TicketArrivalTimeSameDateWithPlanTime,
        ConstraintId::RelationTicketsSameAgent,
        ConstraintId::MinimizeTravelTime,
        ConstraintId::MinimizeTravelDistance,
        ConstraintId::MinimizeAgentFixedCost,
        ConstraintId::SameDepo,
        ConstraintId::BalanceAgentLoading,
        ConstraintId::BalanceAgentLoadingRatio,
        ConstraintId::BalanceAgentWorkingTime,
        ConstraintId::MinimizeTicketChanging,
        ConstraintId::TicketUnassigned,
        ConstraintId::UnreachableLeg,
        ConstraintId::AgentShiftOvertime,
        ConstraintId::ExecutionConsistency,
        ConstraintId::MinimizeVisitOrderChanging,
        ConstraintId::MinimizeServiceTimeChanging,
    ];

    pub const fn index(self) -> usize {
        self as usize
    }

    /// 对外名称（与 proto 字段名、v1 约束名一致）。
    pub const fn name(self) -> &'static str {
        match self {
            ConstraintId::AgentCapacity => "agent_capacity",
            ConstraintId::AgentMaxTicket => "agent_max_ticket",
            ConstraintId::AgentSkills => "agent_skills_accord_with_ticket_skills",
            ConstraintId::AgentQualification => "agent_qualification_levels_match_ticket",
            ConstraintId::RefTicketAfterDepTicket => "ref_ticket_after_dep_ticket",
            ConstraintId::RefTicketSameAgentWithDepTicket => {
                "ref_ticket_same_agent_with_dep_ticket"
            }
            ConstraintId::ServiceFinishedAfterMaxEndTime => "service_finished_after_max_end_time",
            ConstraintId::TicketStartServiceTimeMatchExpected => {
                "ticket_start_service_time_match_expected"
            }
            ConstraintId::TicketArrivalTimeSameDateWithPlanTime => {
                "ticket_arrival_time_same_date_with_plan_time"
            }
            ConstraintId::RelationTicketsSameAgent => "relation_tickets_same_agent",
            ConstraintId::MinimizeTravelTime => "minimize_travel_time",
            ConstraintId::MinimizeTravelDistance => "minimize_travel_distance",
            ConstraintId::MinimizeAgentFixedCost => "minimize_agent_fixed_cost",
            ConstraintId::SameDepo => "same_depo",
            ConstraintId::BalanceAgentLoading => "balance_agent_loading",
            ConstraintId::BalanceAgentLoadingRatio => "balance_agent_loading_ratio",
            ConstraintId::BalanceAgentWorkingTime => "balance_agent_working_time",
            ConstraintId::MinimizeTicketChanging => "minimize_ticket_changing",
            ConstraintId::TicketUnassigned => "ticket_unassigned",
            ConstraintId::UnreachableLeg => "unreachable_leg",
            ConstraintId::AgentShiftOvertime => "agent_shift_overtime",
            ConstraintId::ExecutionConsistency => "execution_consistency",
            ConstraintId::MinimizeVisitOrderChanging => "minimize_visit_order_changing",
            ConstraintId::MinimizeServiceTimeChanging => "minimize_service_time_changing",
        }
    }

    pub const fn default_weight(self) -> Weight {
        match self {
            ConstraintId::AgentCapacity => Weight::hard(1),
            ConstraintId::AgentMaxTicket => Weight::hard(1),
            ConstraintId::AgentSkills => Weight::hard(1),
            ConstraintId::AgentQualification => Weight::medium(100),
            ConstraintId::RefTicketAfterDepTicket => Weight::hard(1),
            ConstraintId::RefTicketSameAgentWithDepTicket => Weight::ZERO,
            ConstraintId::ServiceFinishedAfterMaxEndTime => Weight::ZERO,
            ConstraintId::TicketStartServiceTimeMatchExpected => Weight::soft(100),
            ConstraintId::TicketArrivalTimeSameDateWithPlanTime => Weight::hard(1),
            ConstraintId::RelationTicketsSameAgent => Weight::medium(50),
            ConstraintId::MinimizeTravelTime => Weight::soft(1),
            ConstraintId::MinimizeTravelDistance => Weight::ZERO,
            ConstraintId::MinimizeAgentFixedCost => Weight::soft(20),
            ConstraintId::SameDepo => Weight::hard(1),
            ConstraintId::BalanceAgentLoading => Weight::medium(1),
            ConstraintId::BalanceAgentLoadingRatio => Weight::ZERO,
            ConstraintId::BalanceAgentWorkingTime => Weight::ZERO,
            ConstraintId::MinimizeTicketChanging => Weight::soft(1000),
            ConstraintId::TicketUnassigned => Weight::medium(1000),
            ConstraintId::UnreachableLeg => Weight::hard(1),
            ConstraintId::AgentShiftOvertime => Weight::soft(100),
            ConstraintId::ExecutionConsistency => Weight::hard(1),
            ConstraintId::MinimizeVisitOrderChanging => Weight::soft(10),
            ConstraintId::MinimizeServiceTimeChanging => Weight::soft(1),
        }
    }

    pub fn from_name(name: &str) -> Option<ConstraintId> {
        Self::ALL.into_iter().find(|c| c.name() == name)
    }
}

/// 全部约束的权重，按 [`ConstraintId::index`] 索引。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Weights([Weight; CONSTRAINT_COUNT]);

impl Default for Weights {
    fn default() -> Self {
        Weights(ConstraintId::ALL.map(ConstraintId::default_weight))
    }
}

impl Weights {
    /// 全部关闭，测试里用来单独打开某几条约束。
    pub fn none() -> Self {
        Weights([Weight::ZERO; CONSTRAINT_COUNT])
    }

    #[inline]
    pub fn get(&self, constraint: ConstraintId) -> Weight {
        self.0[constraint.index()]
    }

    pub fn set(&mut self, constraint: ConstraintId, weight: Weight) {
        self.0[constraint.index()] = weight;
    }

    pub fn with(mut self, constraint: ConstraintId, weight: Weight) -> Self {
        self.set(constraint, weight);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_round_trip() {
        for c in ConstraintId::ALL {
            assert_eq!(ConstraintId::from_name(c.name()), Some(c));
            assert_eq!(ConstraintId::ALL[c.index()], c);
        }
    }
}
