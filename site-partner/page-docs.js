// Docs: where publishers learn to build, review, list and price an app (developers.pigeonpost.dev).
(function () {
  "use strict";
  const PP = window.PP, PPP = window.PPP, ui = PP.ui;
  const el = (...a) => PP.el(...a);
  const DEV = "https://developers.pigeonpost.dev/";
  const DOCS = [
    ["nest", "Build a Nest", "What a Nest app is, and how it runs for a buyer.", "nests.html"],
    ["chevron", "Quick start", "From an empty repository to your first build.", "nest-quickstart.html"],
    ["form", "Manifest, operations and configuration", "nest-release.toml, operations and settings.", "nest-manifest.html"],
    ["review", "Security and review", "The review stages and what each one checks.", "nest-review.html"],
    ["tag", "Publish, update and price", "Listings, versions, card prices and per-job prices.", "nest-publish.html"],
    ["apps", "Listing logo and look", "Logos, categories and the catalog card.", "nest-listing.html"],
    ["webhook", "Publisher-hosted apps", "Run jobs on your own servers through a service mailbox.", "nest-hosted.html"],
    ["server", "Runtime API", "What your Nest can call while it runs.", "nest-runtime.html"],
  ];
  PPP.DOCS = DOCS.map(([, title, , file]) => [title, DEV + file]);
  PPP.page({
    key: "docs", title: "Docs", group: "Help", icon: "docs", order: 8,
    render(view) {
      view.append(ui.header({ title: "Docs", text: "Guides for building, reviewing, listing and pricing your apps.", actions: ui.button("developers.pigeonpost.dev", { href: DEV, icon: "external" }) }));
      view.append(el("div", { class: "ppp-docs" }, DOCS.map(([icon, title, text, file]) =>
        el("a", { class: "ppp-doc", href: DEV + file, target: "_blank", rel: "noopener" },
          el("span", { class: "ppp-link-icon" }, PP.icon(icon)),
          el("span", {}, el("strong", {}, title), el("span", { class: "pp-muted" }, text)),
          PP.icon("external", "ppp-doc-ext")))));
      view.append(ui.card({ title: "Need a hand?", text: "The partner team answers in Support, usually within a working day.", actions: ui.button("Open support", { href: PP.href("support"), icon: "support" }) }));
    },
  });
})();
