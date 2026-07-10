// Session.swift
// 一个 Sylvander tab 对应一个 Session。
// session 在服务端持久化;关 tab 只关 view,server 端 session 保留。

import Foundation

struct Session: Identifiable, Equatable, Hashable {
    let id: String           // 服务端给的 UUID
    var title: String        // 第一句 user message,或服务端生成
    var lastActive: Date
    var components: [Component]   // 已渲染的 component 序列
}