//! goal 命令（目标驱动运行）相关测试。

use super::*;

#[test]
fn goal_starts_and_cancels_objective_driven_run() {
    let mut app = app();
    // 无进行中目标：goal 无参只给用法提示，不产生效果
    assert!(app.goal_objective().is_none());
    assert!(app.execute_command(CommandAction::Goal(None)).is_empty());

    // goal <目标>：启动目标驱动运行——记录目标、进入运行态、发出 StartGoal
    let [Effect::StartGoal(objective)] =
        &app.execute_command(CommandAction::Goal(Some("修复测试".to_string())))[..]
    else {
        panic!("expected StartGoal effect");
    };
    assert_eq!(objective, "修复测试");
    assert_eq!(app.goal_objective(), Some("修复测试"));
    assert!(app.running);

    // 已有目标时再次 goal <目标>：替换目标并重启
    app.running = false;
    let [Effect::StartGoal(objective)] =
        &app.execute_command(CommandAction::Goal(Some("新目标".to_string())))[..]
    else {
        panic!("expected StartGoal effect");
    };
    assert_eq!(objective, "新目标");
    assert_eq!(app.goal_objective(), Some("新目标"));

    // goal 无参：取消进行中的目标——清除状态并发出 CancelGoal
    app.running = false;
    let [Effect::CancelGoal] = &app.execute_command(CommandAction::Goal(None))[..] else {
        panic!("expected CancelGoal effect");
    };
    assert!(app.goal_objective().is_none());

    // 本地性：启动属会话命令（运行中拒绝），取消是本地命令（运行中可执行）
    assert!(!CommandAction::Goal(Some("目标".to_string())).is_local());
    assert!(CommandAction::Goal(None).is_local());
}

#[test]
fn parse_command_goal_takes_free_text_objective() {
    assert_eq!(
        parse_command("goal"),
        CommandParse::Known(CommandAction::Goal(None))
    );
    // 空白分隔的自由文本（可含空格）
    assert_eq!(
        parse_command("goal 修复 全部 测试"),
        CommandParse::Known(CommandAction::Goal(Some("修复 全部 测试".to_string())))
    );
    // 冒号形式同样接受
    assert_eq!(
        parse_command("goal:fix all tests"),
        CommandParse::Known(CommandAction::Goal(Some("fix all tests".to_string())))
    );
    // 空参数等价于无参（取消）
    assert_eq!(
        parse_command("goal "),
        CommandParse::Known(CommandAction::Goal(None))
    );
    // 前缀不等于命令名：goalx 报未知
    assert_eq!(
        parse_command("goalx"),
        CommandParse::Unknown("goalx".to_string())
    );
}
