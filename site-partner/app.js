// partner.pigeonpost.dev — boots the shared shell with the partner pages (partner-core.js and
// page-*.js register them in PPP.pages).
(function () {
  "use strict";
  const PP = window.PP, PPP = window.PPP;
  PP.shell.start({
    platform: "partner",
    title: "Partner",
    intro: {
      heading: "Publish on Pigeonpost",
      text: "Apply once, then sell your apps and services to every Pigeonpost user and team.",
      points: ["A guided application for companies and individual developers", "Listings, logos and prices you control", "Usage and earnings for every app, paid monthly", "Live support from the Pigeonpost partner team"],
      footnote: "Use the same Pigeonpost account you use for your inbox.",
    },
    context: PPP.context,
    pages: PPP.pages.slice().sort((a, b) => (a.order ?? 99) - (b.order ?? 99)),
    footLinks: [
      { label: "Developer docs", href: "https://developers.pigeonpost.dev/nests.html", icon: "docs", external: true },
      { label: "Partner directory", href: "https://pigeonpost.dev/partners.html", icon: "external", external: true },
    ],
  });
})();
