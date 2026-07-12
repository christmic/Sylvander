import SwiftUI

/// Bottom warning shown only when the lowest remaining rate-limit
/// percentage is ≤ 15 %. Per checklist §8: "Show bottom warning only
/// at 15% or lower. Copy: 接近速率限制 · 剩余 N%. Do not show a
/// settings action unless it opens a real implemented destination."
struct RateLimitWarning: View {
    var remainingPercent: Double

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(T.warningAmber)
            Text("接近速率限制 · 剩余 \(Fmt.percent(remainingPercent))")
                .font(.system(size: 11, weight: .medium))
                .foregroundStyle(T.textPrimary)
            Spacer()
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(
            RoundedRectangle(cornerRadius: L.rowRadius, style: .continuous)
                .fill(T.warningAmber.opacity(0.10))
        )
        .overlay(
            RoundedRectangle(cornerRadius: L.rowRadius, style: .continuous)
                .strokeBorder(T.warningAmber.opacity(0.40), lineWidth: L.hairline)
        )
    }
}