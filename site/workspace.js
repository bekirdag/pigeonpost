(function () {
  "use strict";
  const MP = (window.PPMarketplace = window.PPMarketplace || {});
  const states = {
    draft: [
      "Draft",
      "neutral",
      "Complete your business details and submit your application.",
    ],
    submitted: [
      "Application submitted",
      "info",
      "Your application is in the review queue. We’ll show updates and any requests here.",
    ],
    in_review: [
      "Under review",
      "info",
      "Pigeonpost is reviewing your application. No action is needed right now.",
    ],
    needs_information: [
      "Action needed",
      "warning",
      "Review the request below, add the missing information, then submit again.",
    ],
    approved: [
      "Approved to publish",
      "success",
      "Your business is verified. Head to My apps to prepare your first release.",
    ],
    rejected: [
      "Application not approved",
      "danger",
      "Read the review decision below. Contact support if you need help with the next step.",
    ],
    revoked: [
      "Publishing suspended",
      "danger",
      "Read the decision and contact support to discuss your business status.",
    ],
  };
  MP.applicationStatus = function (state) {
    const v = Object.hasOwn(states, state)
      ? states[state]
      : [
          "Status unavailable",
          "neutral",
          "Refresh this page to check your application.",
        ];
    return { label: v[0], tone: v[1], hint: v[2] };
  };
  MP.applicationHero = function (app) {
    const s = MP.applicationStatus(app.state),
      e = MP.el;
    return e(
      "section",
      { class: "workspace-hero", "aria-label": "Application status" },
      e(
        "div",
        {},
        e("p", { class: "workspace-eyebrow", text: "Business application" }),
        e("span", { class: "workspace-badge tone-" + s.tone, text: s.label }),
        e("h3", {
          text:
            app.details?.public_name ||
            app.details?.legal_name ||
            "Your publishing journey",
        }),
        e("p", { text: s.hint }),
      ),
      e(
        "div",
        { class: "workspace-hero-actions" },
        app.state === "approved"
          ? e("a", {
              href: "#apps",
              class: "btn btn-primary",
              text: "Go to my apps →",
            })
          : null,
        e("a", {
          href: "#support",
          class: "btn btn-secondary",
          text: "Contact support",
        }),
      ),
    );
  };
  MP.workspaceLinks = function () {
    const e = MP.el;
    return e(
      "div",
      { class: "workspace-links" },
      [
        [
          "business",
          "▦",
          "My business",
          "Application, verification and your public profile",
        ],
        ["apps", "◇", "My apps", "Build, review and publish your releases"],
        [
          "analytics",
          "↗",
          "My analytics",
          "Usage, revenue and payouts across your apps",
        ],
        [
          "support",
          "↔",
          "Support",
          "A direct conversation with the Pigeonpost team",
        ],
      ].map(([route, symbol, title, hint]) =>
        e(
          "a",
          { class: "workspace-link", href: "#" + route },
          e("b", { "aria-hidden": "true", text: symbol }),
          e("strong", { text: title }),
          e("span", { text: hint }),
        ),
      ),
    );
  };
})();
