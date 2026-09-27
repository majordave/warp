use std::time::Duration;

use warp_multi_agent_api::request::input::tool_call_result::Result as ToolCallResult;
use warp_multi_agent_api::run_shell_command_result::Result as RunShellCommandResult;
use warpui::integration::TestStep;
use warpui::{AppContext, SingletonEntity, async_assert};

use crate::ai::agent::conversation::AIConversationId;
use crate::ai::agent::task::TaskId;
use crate::ai::agent::{
    AIAgentAction, AIAgentActionId, AIAgentActionResultType, AIAgentActionType,
    RequestCommandOutputResult,
};
use crate::ai::blocklist::BlocklistAIHistoryModel;
use crate::integration_testing::step::new_step_with_default_assertions;
use crate::integration_testing::view_getters::{single_terminal_view_for_tab, workspace_view};
use crate::terminal::TerminalView;

pub fn add_shared_ambient_docker_sandbox_tab() -> TestStep {
    new_step_with_default_assertions("Open shared ambient Docker sandbox tab").with_action(
        |app, window_id, _| {
            workspace_view(app, window_id).update(app, |workspace, ctx| {
                workspace.add_shared_ambient_docker_sandbox_tab_for_integration_test(ctx);
            });
        },
    )
}

pub fn wait_for_tab_count(expected_tab_count: usize) -> TestStep {
    new_step_with_default_assertions("Wait for Docker sandbox tab")
        .set_timeout(Duration::from_secs(30))
        .add_assertion(move |app, window_id| {
            let tab_count =
                workspace_view(app, window_id).read(app, |workspace, _| workspace.tab_count());
            async_assert!(tab_count == expected_tab_count)
        })
}

pub fn execute_agent_shell_exit(tab_index: usize, command: &'static str) -> TestStep {
    TestStep::new("Execute agent shell exit").with_action(move |app, window_id, data| {
        let terminal = single_terminal_view_for_tab(app, window_id, tab_index);
        let terminal_view_id = terminal.id();
        let conversation_id = BlocklistAIHistoryModel::handle(app).update(app, |history, ctx| {
            let conversation_id =
                history.start_new_conversation(terminal_view_id, false, false, false, ctx);
            history.set_active_conversation_id(conversation_id, terminal_view_id, ctx);
            conversation_id
        });
        let action_id = AIAgentActionId::from(format!("shell-recovery-{conversation_id}"));
        data.insert("recovery_action", (conversation_id, action_id.clone()));
        let action_model = terminal.read(app, |terminal, _| terminal.ai_action_model().clone());
        action_model.update(app, |model, ctx| {
            model.queue_action_for_integration_test(
                AIAgentAction {
                    id: action_id,
                    task_id: TaskId::new(format!("shell-recovery-{conversation_id}")),
                    action: AIAgentActionType::RequestCommandOutput {
                        command: command.to_owned(),
                        is_read_only: Some(false),
                        is_risky: Some(false),
                        wait_until_completion: true,
                        uses_pager: Some(false),
                        rationale: None,
                        citations: Vec::new(),
                    },
                    requires_result: true,
                },
                conversation_id,
                ctx,
            );
        });
    })
}

fn recovered_command_result(
    terminal: &TerminalView,
    conversation_id: AIConversationId,
    action_id: &AIAgentActionId,
    ctx: &AppContext,
) -> Option<(usize, ToolCallResult)> {
    let results = terminal
        .ai_action_model()
        .as_ref(ctx)
        .get_finished_action_results(conversation_id)?;
    let mut results = results.iter().filter(|result| &result.id == action_id);
    let result = results.next()?;
    let AIAgentActionResultType::RequestCommandOutput(
        result @ RequestCommandOutputResult::ShellRecovered { .. },
    ) = &result.result
    else {
        return None;
    };
    Some((
        1 + results.count(),
        ToolCallResult::try_from(result.clone()).ok()?,
    ))
}

pub fn wait_for_recovery(tab_index: usize) -> TestStep {
    new_step_with_default_assertions("Wait for shell recovery")
        .set_timeout(Duration::from_secs(30))
        .add_named_assertion_with_data_from_prior_step(
            "Recovery delivers once and retains sharing",
            move |app, window_id, data| {
                let (conversation_id, action_id) = data
                    .get::<_, (AIConversationId, AIAgentActionId)>("recovery_action")
                    .expect("agent command was queued");
                single_terminal_view_for_tab(app, window_id, tab_index).read(
                    app,
                    |terminal, ctx| {
                        let result =
                            recovered_command_result(terminal, *conversation_id, action_id, ctx);
                        let model = terminal.model.lock();
                        async_assert!(
                            matches!(result, Some((1, _)))
                                && model.shared_session_status().is_active_sharer()
                                && model.is_shared_ambient_agent_session()
                        )
                    },
                )
            },
        )
}
pub fn wait_for_shared_ambient_session(tab_index: usize) -> TestStep {
    new_step_with_default_assertions("Wait for shared ambient session")
        .set_timeout(Duration::from_secs(30))
        .add_assertion(move |app, window_id| {
            single_terminal_view_for_tab(app, window_id, tab_index).read(app, |terminal, _| {
                let model = terminal.model.lock();
                async_assert!(
                    model.shared_session_status().is_active_sharer()
                        && model.is_shared_ambient_agent_session()
                )
            })
        })
}

pub fn wait_for_recovered_command_result(
    tab_index: usize,
    expected_status: &'static str,
    expected_exit_code: i32,
) -> TestStep {
    new_step_with_default_assertions("Wait for external recovered command result")
        .set_timeout(Duration::from_secs(30))
        .add_named_assertion_with_data_from_prior_step(
            "Recovered command preserves observed exit status",
            move |app, window_id, data| {
                let (conversation_id, action_id) = data
                    .get::<_, (AIConversationId, AIAgentActionId)>("recovery_action")
                    .expect("agent command was queued");
                let result = single_terminal_view_for_tab(app, window_id, tab_index).read(
                    app,
                    |terminal, ctx| {
                        recovered_command_result(terminal, *conversation_id, action_id, ctx)
                    },
                );
                let Some((delivery_count, ToolCallResult::RunShellCommand(result))) = result else {
                    return async_assert!(false, "recovered command result not available");
                };
                let Some(RunShellCommandResult::CommandFinished(command_finished)) = result.result
                else {
                    return async_assert!(false, "expected command_finished result");
                };
                let has_expected_status = command_finished.output.contains(expected_status);
                async_assert!(
                    delivery_count == 1
                        && has_expected_status
                        && command_finished.exit_code == expected_exit_code,
                    "delivery_count={delivery_count}, output={:?}, exit_code={}",
                    command_finished.output,
                    command_finished.exit_code,
                )
            },
        )
}
