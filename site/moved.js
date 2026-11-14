// pigeonpost.dev/account and /staff moved to their own platforms (plan 2026-11-14-platform-
// separation). nginx serves account-moved.html for /account and staff-moved.html for /staff; this
// script forwards to the right host, keeping the query (payment returns: ?credits=, ?card=, …)
// and turning the old section fragment into the new route. Nothing is read beyond the URL.
(function () {
  "use strict";
  var loc = window.location;
  var q = loc.search || "";
  var hash = (loc.hash || "").replace(/^#\/?/, "");
  var first = hash.split(/[/?]/)[0];
  var rest = hash.slice(first.length);
  var params = new URLSearchParams(q);
  var target;
  if (document.documentElement.getAttribute("data-moved") === "staff") {
    var STAFF = { dashboard: "overview", orgs: "enterprises", analytics: "partner-analytics", publishers: "applications", abuse: "reports", plans: "plans" };
    var key = STAFF[first] || first;
    target = "https://management.pigeonpost.dev/" + q + (key ? "#/" + key + rest : "");
  } else {
    var PARTNER = { analytics: "analytics", business: "business", apps: "apps", support: "support", publish: "application", publisher: "application" };
    if (Object.prototype.hasOwnProperty.call(PARTNER, first)) {
      target = "https://partner.pigeonpost.dev/" + q + "#/" + PARTNER[first] + rest;
    } else if (params.get("nest_org")) {
      // A company Nest bought from enterprise. returns here (the bridge's return address).
      target = "https://enterprise.pigeonpost.dev/app" + q + "#/nests";
    } else {
      // account. understands the old section names and every payment-return parameter itself.
      target = "https://account.pigeonpost.dev/" + q + (hash ? "#" + hash : "");
    }
  }
  var a = document.getElementById("moved-link");
  if (a) a.setAttribute("href", target);
  loc.replace(target);
})();
