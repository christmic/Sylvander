# `sylvander-work`

`sylvander-work/src-tauri` is the desktop application shell. It owns desktop
process startup, Tauri commands and application packaging. It consumes public
Runtime/API contracts and must not become an alternate owner of Agent loops,
durable Session state, provider protocols, coordination policy, or workspace
execution.

Desktop presentation state belongs in the frontend; authentication,
authorization, persistence, recovery, and governance decisions remain in
Runtime. Production crates never depend on the desktop package.
