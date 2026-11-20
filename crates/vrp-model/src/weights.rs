//! `ConstraintId` 与 proto `ConstraintWeights` 字段的对应关系。

use vrp_core::constraint::ConstraintId as C;
use vrp_core::{Weight, Weights};
use vrp_proto::engine::v1 as pb;

pub fn weight_slot(cw: &pb::ConstraintWeights, c: C) -> Option<&pb::Weight> {
    match c {
        C::AgentCapacity => cw.agent_capacity.as_ref(),
        C::AgentMaxTicket => cw.agent_max_ticket.as_ref(),
        C::AgentSkills => cw.agent_skills_accord_with_ticket_skills.as_ref(),
        C::AgentQualification => cw.agent_qualification_levels_match_ticket.as_ref(),
        C::RefTicketAfterDepTicket => cw.ref_ticket_after_dep_ticket.as_ref(),
        C::RefTicketSameAgentWithDepTicket => cw.ref_ticket_same_agent_with_dep_ticket.as_ref(),
        C::ServiceFinishedAfterMaxEndTime => cw.service_finished_after_max_end_time.as_ref(),
        C::TicketStartServiceTimeMatchExpected => {
            cw.ticket_start_service_time_match_expected.as_ref()
        }
        C::TicketArrivalTimeSameDateWithPlanTime => {
            cw.ticket_arrival_time_same_date_with_plan_time.as_ref()
        }
        C::RelationTicketsSameAgent => cw.relation_tickets_same_agent.as_ref(),
        C::MinimizeTravelTime => cw.minimize_travel_time.as_ref(),
        C::MinimizeTravelDistance => cw.minimize_travel_distance.as_ref(),
        C::MinimizeAgentFixedCost => cw.minimize_agent_fixed_cost.as_ref(),
        C::SameDepo => cw.same_depo.as_ref(),
        C::BalanceAgentLoading => cw.balance_agent_loading.as_ref(),
        C::BalanceAgentLoadingRatio => cw.balance_agent_loading_ratio.as_ref(),
        C::BalanceAgentWorkingTime => cw.balance_agent_working_time.as_ref(),
        C::MinimizeTicketChanging => cw.minimize_ticket_changing.as_ref(),
        C::TicketUnassigned => cw.ticket_unassigned.as_ref(),
        C::UnreachableLeg => cw.unreachable_leg.as_ref(),
        C::AgentShiftOvertime => cw.agent_shift_overtime.as_ref(),
        C::ExecutionConsistency => cw.execution_consistency.as_ref(),
        C::MinimizeVisitOrderChanging => cw.minimize_visit_order_changing.as_ref(),
        C::MinimizeServiceTimeChanging => cw.minimize_service_time_changing.as_ref(),
    }
}

pub fn weight_slot_mut(cw: &mut pb::ConstraintWeights, c: C) -> &mut Option<pb::Weight> {
    match c {
        C::AgentCapacity => &mut cw.agent_capacity,
        C::AgentMaxTicket => &mut cw.agent_max_ticket,
        C::AgentSkills => &mut cw.agent_skills_accord_with_ticket_skills,
        C::AgentQualification => &mut cw.agent_qualification_levels_match_ticket,
        C::RefTicketAfterDepTicket => &mut cw.ref_ticket_after_dep_ticket,
        C::RefTicketSameAgentWithDepTicket => &mut cw.ref_ticket_same_agent_with_dep_ticket,
        C::ServiceFinishedAfterMaxEndTime => &mut cw.service_finished_after_max_end_time,
        C::TicketStartServiceTimeMatchExpected => &mut cw.ticket_start_service_time_match_expected,
        C::TicketArrivalTimeSameDateWithPlanTime => {
            &mut cw.ticket_arrival_time_same_date_with_plan_time
        }
        C::RelationTicketsSameAgent => &mut cw.relation_tickets_same_agent,
        C::MinimizeTravelTime => &mut cw.minimize_travel_time,
        C::MinimizeTravelDistance => &mut cw.minimize_travel_distance,
        C::MinimizeAgentFixedCost => &mut cw.minimize_agent_fixed_cost,
        C::SameDepo => &mut cw.same_depo,
        C::BalanceAgentLoading => &mut cw.balance_agent_loading,
        C::BalanceAgentLoadingRatio => &mut cw.balance_agent_loading_ratio,
        C::BalanceAgentWorkingTime => &mut cw.balance_agent_working_time,
        C::MinimizeTicketChanging => &mut cw.minimize_ticket_changing,
        C::TicketUnassigned => &mut cw.ticket_unassigned,
        C::UnreachableLeg => &mut cw.unreachable_leg,
        C::AgentShiftOvertime => &mut cw.agent_shift_overtime,
        C::ExecutionConsistency => &mut cw.execution_consistency,
        C::MinimizeVisitOrderChanging => &mut cw.minimize_visit_order_changing,
        C::MinimizeServiceTimeChanging => &mut cw.minimize_service_time_changing,
    }
}

/// 未设置的约束取默认权重。
pub fn weights_from_proto(cw: Option<&pb::ConstraintWeights>) -> Weights {
    let mut weights = Weights::default();
    if let Some(cw) = cw {
        for c in C::ALL {
            if let Some(w) = weight_slot(cw, c) {
                weights.set(
                    c,
                    Weight {
                        hard: w.hard,
                        medium: w.medium,
                        soft: w.soft,
                    },
                );
            }
        }
    }
    weights
}
