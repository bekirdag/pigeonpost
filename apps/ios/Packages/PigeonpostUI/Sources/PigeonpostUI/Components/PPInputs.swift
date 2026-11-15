//  Inputs: PPTextField (`.pp-field` + `.pp-input`), PPOTPField (`.pp-otp`), PPToggleRow
//  (`.pp-switch` in a settings row).

import SwiftUI

/// A labelled text field with hint, error, optional leading icon and secure entry. The error
/// replaces the hint, turns the border red and is read out after the label.
public struct PPTextField: View {
    let label: String
    let placeholder: String
    @Binding var text: String
    let hint: String?
    let error: String?
    let icon: PPIconName?
    let isSecure: Bool
    let isRequired: Bool
    let mono: Bool

    @FocusState private var focused: Bool
    @Environment(\.isEnabled) private var isEnabled

    public init(_ label: String, text: Binding<String>, placeholder: String = "", hint: String? = nil,
                error: String? = nil, icon: PPIconName? = nil, isSecure: Bool = false,
                isRequired: Bool = false, mono: Bool = false) {
        self.label = label
        self._text = text
        self.placeholder = placeholder
        self.hint = hint
        self.error = error
        self.icon = icon
        self.isSecure = isSecure
        self.isRequired = isRequired
        self.mono = mono
    }

    private var invalid: Bool { error != nil }

    public var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 4) {
                Text(label).font(.geist(14, .medium)).foregroundStyle(PP.Colors.text)
                if isRequired { Text("*").font(.geist(14, .semibold)).foregroundStyle(PP.Colors.danger).accessibilityHidden(true) }
            }
            let shape = RoundedRectangle(cornerRadius: PP.Radius.m, style: .continuous)
            HStack(spacing: 8) {
                if let icon { PPIcon(icon, size: 18).foregroundStyle(PP.Colors.textFaint) }
                Group {
                    if isSecure {
                        SecureField("", text: $text, prompt: Text(placeholder).foregroundStyle(PP.Colors.textFaint))
                    } else {
                        TextField("", text: $text, prompt: Text(placeholder).foregroundStyle(PP.Colors.textFaint))
                    }
                }
                .textFieldStyle(.plain)
                .font(mono ? .geistMono(15) : .geist(15))
                .foregroundStyle(PP.Colors.text)
                .focused($focused)
                .accessibilityLabel(Text(label))
                .accessibilityHint(Text(error ?? hint ?? ""))
            }
            .padding(.horizontal, 12)
            .frame(minHeight: 44)
            .background(shape.fill(isEnabled ? (invalid ? PP.Colors.dangerFill.opacity(0.35) : PP.Colors.surface) : PP.Colors.surfaceSunken))
            .overlay(shape.strokeBorder(invalid ? PP.Colors.danger : (focused ? PP.Colors.accent : PP.Colors.borderStrong), lineWidth: 1))
            .ppFocusRing(focused, in: shape)
            .ppShadow(.one)
            .animation(.easeOut(duration: PP.Duration.fast), value: focused)
            if let error {
                Label { Text(error) } icon: { PPIcon(.alertCircle, size: 15) }
                    .font(.geist(13)).foregroundStyle(PP.Colors.danger)
                    .labelStyle(PPInlineLabelStyle())
            } else if let hint {
                Text(hint).font(.geist(13)).foregroundStyle(PP.Colors.textMuted)
            }
        }
        .opacity(isEnabled ? 1 : 0.6)
    }
}

struct PPInlineLabelStyle: LabelStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            configuration.icon.alignmentGuide(.firstTextBaseline) { $0[.bottom] - 2 }
            configuration.title
        }
    }
}

/// A one-time code field: one hidden text field drives `length` cells, so paste and SMS
/// autofill (`.oneTimeCode`) work. Cells are 46 x 54, Geist Mono 22.
public struct PPOTPField: View {
    @Binding var code: String
    let length: Int
    let isInvalid: Bool
    let onComplete: ((String) -> Void)?
    @FocusState private var focused: Bool

    public init(code: Binding<String>, length: Int = 6, isInvalid: Bool = false, onComplete: ((String) -> Void)? = nil) {
        self._code = code
        self.length = length
        self.isInvalid = isInvalid
        self.onComplete = onComplete
    }

    public var body: some View {
        ZStack {
            TextField("", text: $code)
                #if os(iOS)
                .keyboardType(.numberPad)
                .textContentType(.oneTimeCode)
                #endif
                .focused($focused)
                .opacity(0.02)
                .onChange(of: code) { _, new in
                    let digits = String(new.filter(\.isNumber).prefix(length))
                    if digits != new { code = digits }
                    if digits.count == length { onComplete?(digits) }
                }
                .accessibilityLabel(Text("Verification code, \(length) digits"))
                .accessibilityValue(Text(code.map(String.init).joined(separator: " ")))
            HStack(spacing: 8) {
                ForEach(0..<length, id: \.self) { i in
                    cell(i)
                    if length == 6 && i == 2 {
                        Capsule().fill(PP.Colors.borderStrong).frame(width: 10, height: 2)
                    }
                }
            }
            .allowsHitTesting(false)
            .accessibilityHidden(true)
        }
        .contentShape(Rectangle())
        .onTapGesture { focused = true }
    }

    private func cell(_ i: Int) -> some View {
        let chars = Array(code)
        let ch = i < chars.count ? String(chars[i]) : ""
        let active = focused && (i == min(chars.count, length - 1))
        let shape = RoundedRectangle(cornerRadius: PP.Radius.m, style: .continuous)
        return Text(ch)
            .font(.geistMono(22, .semibold))
            .monospacedDigit()
            .foregroundStyle(PP.Colors.text)
            .frame(width: 46, height: 54)
            .background(shape.fill(isInvalid ? PP.Colors.dangerFill : PP.Colors.surface))
            .overlay(shape.strokeBorder(isInvalid ? PP.Colors.danger : (active ? PP.Colors.accent : (ch.isEmpty ? PP.Colors.borderStrong : PP.Colors.textFaint.opacity(0.6))), lineWidth: 1))
            .ppFocusRing(active, in: shape)
            .ppShadow(.one)
    }
}

/// A settings row with a title, optional detail and icon, and a Pigeonpost switch (36 x 22).
public struct PPToggleRow: View {
    let title: String
    let detail: String?
    let icon: PPIconName?
    @Binding var isOn: Bool

    public init(_ title: String, detail: String? = nil, icon: PPIconName? = nil, isOn: Binding<Bool>) {
        self.title = title
        self.detail = detail
        self.icon = icon
        self._isOn = isOn
    }

    public var body: some View {
        Toggle(isOn: $isOn) {
            HStack(spacing: 12) {
                if let icon {
                    PPIcon(icon, size: 18)
                        .foregroundStyle(PP.Colors.textMuted)
                        .frame(width: 36, height: 36)
                        .background(RoundedRectangle(cornerRadius: PP.Radius.m).fill(PP.Colors.surfaceSunken))
                }
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(.geist(15, .medium)).foregroundStyle(PP.Colors.text)
                    if let detail { Text(detail).font(.geist(13.5)).foregroundStyle(PP.Colors.textMuted) }
                }
            }
        }
        .toggleStyle(PPSwitchStyle())
        .padding(.vertical, 12)
        .padding(.horizontal, 16)
        .frame(minHeight: 52)
    }
}

/// The `.pp-switch` look: 36 x 22 track, 18pt thumb, accent when on.
public struct PPSwitchStyle: ToggleStyle {
    @Environment(\.isEnabled) private var isEnabled
    public init() {}

    public func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: 12) {
            configuration.label
            Spacer(minLength: 8)
            ZStack(alignment: configuration.isOn ? .trailing : .leading) {
                Capsule().fill(configuration.isOn ? PP.Colors.accent : PP.Colors.borderStrong)
                Circle().fill(PP.Colors.controlThumb)
                    .frame(width: 18, height: 18)
                    .shadow(color: .black.opacity(0.25), radius: 1.5, y: 1)
                    .padding(2)
            }
            .frame(width: 36, height: 22)
            .animation(.easeInOut(duration: PP.Duration.base), value: configuration.isOn)
        }
        .contentShape(Rectangle())
        .onTapGesture { if isEnabled { configuration.isOn.toggle() } }
        .opacity(isEnabled ? 1 : 0.5)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
        .accessibilityValue(Text(configuration.isOn ? "On" : "Off"))
    }
}

#Preview("Inputs") {
    struct Demo: View {
        @State var handle = "/ada"
        @State var email = "ada@wodo"
        @State var code = "4821"
        @State var push = true
        @State var agents = false
        var body: some View {
            ScrollView {
                VStack(alignment: .leading, spacing: 20) {
                    PPTextField("Your handle", text: $handle, placeholder: "/name", hint: "People reach you at this address.", icon: .atSign, isRequired: true, mono: true)
                    PPTextField("Email", text: $email, placeholder: "you@example.com", error: "Enter a full email address, like ada@wodo.io.", icon: .mail)
                    PPTextField("Password", text: .constant(""), placeholder: "At least 12 characters", isSecure: true)
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Enter the code we sent to ada@wodo.io").ppText(.bodyS).foregroundStyle(PP.Colors.textMuted)
                        PPOTPField(code: $code)
                        PPOTPField(code: .constant("123456"), isInvalid: true)
                    }
                    VStack(spacing: 0) {
                        PPToggleRow("Push notifications", detail: "New messages and calls", icon: .bell, isOn: $push)
                        Divider().overlay(PP.Colors.borderSubtle)
                        PPToggleRow("Let agents start calls", detail: "Only agents you have approved", icon: .bot, isOn: $agents)
                    }
                    .background(RoundedRectangle(cornerRadius: PP.Radius.l).fill(PP.Colors.surface))
                }
                .padding(20)
            }
            .background(PP.Colors.bg)
        }
    }
    return Demo().pigeonpostTheme()
}
