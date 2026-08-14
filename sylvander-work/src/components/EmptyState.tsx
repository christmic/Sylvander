interface EmptyStateProps {
  workspaceLabel?: string;
  onAction(action: "explore" | "build" | "review"): void;
}

/**
 * Codex-style welcome surface. Shown only when a Session is selected and the
 * transcript is empty. Three action cards match the Codex desktop grammar:
 * icon → short label, hover affordance, keyboard hint.
 */
export function EmptyState({ workspaceLabel, onAction }: EmptyStateProps) {
  const target = workspaceLabel?.trim() ? workspaceLabel : "Sylvander";
  return (
    <section className="empty-state" aria-label={`${target} welcome`}>
      <div className="empty-mark" aria-hidden="true">✺</div>
      <h2 className="empty-title">要在 <span>{target}</span> 内开发什么？</h2>
      <div className="empty-cards" role="group" aria-label="Quick actions">
        <button className="action-card" onClick={() => onAction("explore")} type="button">
          <span className="action-icon" aria-hidden="true">⚲</span>
          <span className="action-label">探索并理解代码</span>
          <span className="action-hint">找出入口 · 阅读历史 · 总结</span>
        </button>
        <button className="action-card" onClick={() => onAction("build")} type="button">
          <span className="action-icon" aria-hidden="true">✦</span>
          <span className="action-label">构建新功能、应用或工具</span>
          <span className="action-hint">规划 · 实施 · 验证</span>
        </button>
        <button className="action-card" onClick={() => onAction("review")} type="button">
          <span className="action-icon" aria-hidden="true">↺</span>
          <span className="action-label">审查代码并提出修改建议</span>
          <span className="action-hint">解释意图 · 指出风险</span>
        </button>
      </div>
    </section>
  );
}
