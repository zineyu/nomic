//! session runner 集成测试（ADR-0033）：串行消费、每 job 独立取消、
//! 空结果翻译与统一生命周期事件。

// 与 agent_loop 共享 mock provider / 构造 helper；本测试只用其中一部分
#[allow(dead_code)]
#[path = "agent_loop/support.rs"]
mod support;

use std::sync::Arc;
use std::time::Duration;

use nomic_ai::{AssistantEvent, Context, Model, Provider, StopReason, StreamOptions};
use nomic_core::{
    Agent, CompactOutcome, ContinueOutcome, JobKind, JobOutcome, NOTHING_TO_COMPACT,
    NOTHING_TO_CONTINUE, RunnerError, RunnerEvent, SessionJob, SessionRunner,
};
use support::{MockProvider, error_done, make_agent, model, text_done};
use tokio_util::sync::CancellationToken;

fn prompt(text: &str) -> SessionJob {
    SessionJob::Prompt {
        text: text.to_string(),
        images: Vec::new(),
    }
}

/// 收取下一条 runner 事件（测试辅助：超时兜底防挂起）。
async fn next_event(events: &mut tokio::sync::mpsc::UnboundedReceiver<RunnerEvent>) -> RunnerEvent {
    tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("runner 事件不应超时")
        .expect("runner 事件通道不应关闭")
}

/// 收取下一条 Finished 的 outcome（跳过前面的 Started）。
async fn next_outcome(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<RunnerEvent>,
) -> JobOutcome {
    loop {
        match next_event(events).await {
            RunnerEvent::Finished(outcome) => return outcome,
            RunnerEvent::Started(_) => {}
        }
    }
}

/// job 串行消费：每个 job 严格按序产生 Started → Finished；第二次
/// prompt 一定看到第一次注入的上下文（队列顺序即执行顺序）。
#[tokio::test]
async fn jobs_run_serially_with_ordered_lifecycle_events() {
    let provider = MockProvider::new(vec![text_done("r1"), text_done("r2")]);
    let (agent, _events) = make_agent(provider.clone(), vec![]);
    let (handle, _actor) = agent.spawn();
    let (runner, mut events, _task) = SessionRunner::spawn(handle);

    runner.submit(prompt("一")).expect("提交应成功");
    runner.submit(prompt("二")).expect("提交应成功");

    let kinds: Vec<JobKind> = vec![
        match next_event(&mut events).await {
            RunnerEvent::Started(kind) => kind,
            RunnerEvent::Finished(_) => panic!("应为 Started"),
        },
        match next_event(&mut events).await {
            RunnerEvent::Finished(JobOutcome::Prompt(Ok(_))) => JobKind::Prompt,
            other => panic!("应为 Prompt Finished：{other:?}"),
        },
        match next_event(&mut events).await {
            RunnerEvent::Started(kind) => kind,
            RunnerEvent::Finished(_) => panic!("应为 Started"),
        },
        match next_event(&mut events).await {
            RunnerEvent::Finished(JobOutcome::Prompt(Ok(outcome))) => {
                assert!(outcome.ended_normally());
                JobKind::Prompt
            }
            other => panic!("应为 Prompt Finished：{other:?}"),
        },
    ];
    assert_eq!(
        kinds,
        vec![
            JobKind::Prompt,
            JobKind::Prompt,
            JobKind::Prompt,
            JobKind::Prompt
        ]
    );
    assert_eq!(
        provider.context_lens(),
        vec![1, 3],
        "第二个 prompt 一定看到第一个 prompt 的消息（串行）"
    );
    assert_eq!(runner.queued_len(), 0);
    assert!(!runner.is_running(), "全部完成后不在运行态");
}

/// prompt 以 Error 收尾时不算正常结束（goal 追问判定依据）。
#[tokio::test]
async fn prompt_ending_with_error_is_not_normal() {
    let provider = MockProvider::new(vec![error_done(StopReason::Error, "boom")]);
    let (agent, _events) = make_agent(provider, vec![]);
    let (handle, _actor) = agent.spawn();
    let (runner, mut events, _task) = SessionRunner::spawn(handle);

    runner.submit(prompt("hi")).expect("提交应成功");
    let JobOutcome::Prompt(Ok(outcome)) = next_outcome(&mut events).await else {
        panic!("loop 内错误经事件收尾，job 本身应成功");
    };
    assert!(!outcome.ended_normally(), "Error 收尾不算正常结束");
}

/// compact 的空结果翻译为 NothingToCompact（不产生 agent 事件，文案
/// 常量化）；有内容时翻译为 Compacted。
#[tokio::test]
async fn compact_outcomes_distinguish_empty_history() {
    let provider = MockProvider::new(vec![]);
    let (agent, _events) = make_agent(provider, vec![]);
    let (handle, _actor) = agent.spawn();
    let (runner, mut events, _task) = SessionRunner::spawn(handle);

    runner
        .submit(SessionJob::Compact { instructions: None })
        .expect("提交应成功");
    let JobOutcome::Compact(Ok(CompactOutcome::NothingToCompact)) = next_outcome(&mut events).await
    else {
        panic!("空历史压缩应为 NothingToCompact");
    };
    assert_eq!(NOTHING_TO_COMPACT, "上下文很短，没有可压缩的内容。");
}

/// continue 的空结果翻译为 NothingToContinue；历史尾部有 user 消息时
/// 翻译为 Continued。
#[tokio::test]
async fn continue_outcomes_distinguish_tail_message() {
    let provider = MockProvider::new(vec![text_done("续")]);
    let (agent, _events) = make_agent(provider, vec![]);
    let (handle, _actor) = agent.spawn();
    let (runner, mut events, _task) = SessionRunner::spawn(handle.clone());

    runner.submit(SessionJob::Continue).expect("提交应成功");
    let JobOutcome::Continue(Ok(ContinueOutcome::NothingToContinue)) =
        next_outcome(&mut events).await
    else {
        panic!("空历史续跑应为 NothingToContinue");
    };
    assert_eq!(NOTHING_TO_CONTINUE, "历史尾部没有可续跑的消息。");

    // 注入一条 user 消息（fire-and-forget，邮箱 FIFO 保证先于续跑生效）
    handle.inject_user_message("继续").expect("注入应成功");
    runner.submit(SessionJob::Continue).expect("提交应成功");
    let JobOutcome::Continue(Ok(ContinueOutcome::Continued)) = next_outcome(&mut events).await
    else {
        panic!("尾部有 user 消息应为 Continued");
    };
}

/// 取消在途 job：阻塞中的 prompt 被中断，结果标记为「非正常结束」；
/// 空闲时取消返回 false。
#[tokio::test]
async fn cancel_current_aborts_running_job() {
    /// stream 挂起直到取消令牌触发的 provider。
    struct BlockingProvider;

    impl Provider for BlockingProvider {
        fn stream(
            &self,
            _model: &Model,
            _context: &Context,
            _options: &StreamOptions,
            cancel: CancellationToken,
        ) -> nomic_ai::AssistantStream {
            let (tx, stream) = nomic_ai::channel();
            tokio::spawn(async move {
                cancel.cancelled().await;
                let _ = tx.send(AssistantEvent::Error {
                    message: Box::new(support::assistant_message(vec![], StopReason::Aborted)),
                });
            });
            stream
        }
    }

    let (agent, _events) = Agent::builder()
        .model(model())
        .provider(Arc::new(BlockingProvider))
        .system_prompt("sys")
        .build();
    let (handle, _actor) = agent.spawn();
    let (runner, mut events, _task) = SessionRunner::spawn(handle);

    assert!(!runner.cancel_current(), "空闲时取消应返回 false");

    runner.submit(prompt("hi")).expect("提交应成功");
    // 等 job 开始执行（Started 到达即在途）
    match next_event(&mut events).await {
        RunnerEvent::Started(JobKind::Prompt) => {}
        other => panic!("应为 Started(Prompt)：{other:?}"),
    }
    assert!(runner.is_running());
    assert!(runner.cancel_current(), "在途取消应生效");

    let JobOutcome::Prompt(Ok(outcome)) = next_outcome(&mut events).await else {
        panic!("取消的 prompt 应正常收尾（Aborted 事件）");
    };
    assert!(!outcome.ended_normally(), "被取消不算正常结束");
}

/// 全部句柄断开后 runner 任务退出，事件通道随之关闭；任务退出后
/// （panic / abort）提交报 `RunnerError::Gone`。
#[tokio::test]
async fn submit_fails_after_runner_task_exits() {
    let provider = MockProvider::new(vec![]);
    let (agent, _events) = make_agent(provider, vec![]);
    let (handle, _actor) = agent.spawn();
    let (runner, mut events, task) = SessionRunner::spawn(handle);

    drop(runner);
    task.await.expect("句柄断开后 runner 应退出");
    assert!(events.recv().await.is_none(), "事件通道随之关闭");

    // 任务退出（abort 模拟 panic）：receiver 随任务丢弃，提交报 Gone
    let provider = MockProvider::new(vec![]);
    let (agent, _events) = make_agent(provider, vec![]);
    let (handle, _actor) = agent.spawn();
    let (runner, _events, task) = SessionRunner::spawn(handle);
    task.abort();
    let _ = task.await;
    let error = runner
        .submit(prompt("hi"))
        .expect_err("任务退出后提交应失败");
    assert!(matches!(error, RunnerError::Gone));
}
