// The emoji picker in the direct and group composers. No network and no image set: the characters
// are drawn by the device's own emoji font, and the only thing remembered is the short recent list.
(function () {
  "use strict";

  const SETS = [
    ["Smileys", "😀", "😀 grinning happy smile|😃 smiley happy|😄 smile happy|😁 beaming grin|😆 laughing|😅 sweat smile relief|🤣 rofl rolling laughing|😂 joy tears laughing|🙂 slight smile|🙃 upside down|😉 wink|😊 blush smile|😇 halo angel|🥰 smiling hearts love|😍 heart eyes love|🤩 star struck|😘 kiss blow|😗 kissing|😚 kissing closed eyes|😋 yum delicious|😛 tongue|😜 wink tongue|🤪 zany crazy|😝 squint tongue|🤑 money mouth|🤗 hug|🤭 hand over mouth|🤫 shush quiet|🤔 thinking|🤐 zipper mouth|🤨 raised eyebrow|😐 neutral|😑 expressionless|😶 no mouth|😏 smirk|😒 unamused|🙄 eye roll|😬 grimace|😌 relieved|😔 pensive|😪 sleepy|🤤 drooling|😴 sleeping|😷 mask sick|🤒 thermometer sick|🤕 bandage hurt|🤢 nauseated|🤮 vomit|🤧 sneeze|🥵 hot|🥶 cold|🥴 woozy|😵 dizzy|🤯 mind blown|🤠 cowboy|🥳 party celebrate|😎 sunglasses cool|🤓 nerd|🧐 monocle|😕 confused|😟 worried|🙁 slight frown|😮 open mouth surprised|😯 hushed|😲 astonished|😳 flushed|🥺 pleading|😦 frowning|😧 anguished|😨 fearful|😰 anxious sweat|😥 sad relieved|😢 cry tear|😭 sob crying|😱 scream|😖 confounded|😣 persevere|😞 disappointed|😓 downcast sweat|😩 weary|😫 tired|🥱 yawn|😤 triumph huff|😡 pout angry|😠 angry|🤬 swearing|😈 devil smile|👿 imp|💀 skull|💩 poop|🤡 clown|👻 ghost|👽 alien|🤖 robot"],
    ["Gestures", "👍", "👍 thumbs up yes like|👎 thumbs down no|👌 ok perfect|🤌 pinched fingers|✌️ victory peace|🤞 crossed fingers luck|🤟 love you|🤘 rock horns|🤙 call me|👈 point left|👉 point right|👆 point up|👇 point down|☝️ index up|👋 wave hello bye|🤚 raised back hand|✋ raised hand stop|🖐️ hand fingers|🖖 vulcan|👏 clap|🙌 raised hands hooray|👐 open hands|🤲 palms up|🤝 handshake deal|🙏 pray thanks please|✍️ writing|💪 muscle strong|🧠 brain|👀 eyes look|👁️ eye|🫶 heart hands|🙋 person raising hand|🤷 shrug|🤦 facepalm|🙇 bow|💁 info person|🙅 no gesture|🙆 ok gesture|🚶 walking|🏃 running|💃 dancing woman|🕺 dancing man|👶 baby|👧 girl|👦 boy|👩 woman|👨 man|🧑 person|👵 old woman|👴 old man"],
    ["Hearts", "❤️", "❤️ red heart love|🧡 orange heart|💛 yellow heart|💚 green heart|💙 blue heart|💜 purple heart|🖤 black heart|🤍 white heart|🤎 brown heart|💔 broken heart|❣️ heart exclamation|💕 two hearts|💞 revolving hearts|💓 beating heart|💗 growing heart|💖 sparkling heart|💘 cupid arrow heart|💝 gift heart|💯 hundred percent|💥 boom collision|💫 dizzy star|💦 sweat drops|💨 dash wind|🔥 fire lit|✨ sparkles|⭐ star|🌟 glowing star|🎉 party popper celebrate|🎊 confetti|🎈 balloon|🎁 gift present|🏆 trophy win|🥇 gold medal|🏅 medal"],
    ["Animals", "🐶", "🐶 dog|🐱 cat|🐭 mouse|🐹 hamster|🐰 rabbit bunny|🦊 fox|🐻 bear|🐼 panda|🐨 koala|🐯 tiger|🦁 lion|🐮 cow|🐷 pig|🐸 frog|🐵 monkey|🙈 see no evil|🙉 hear no evil|🙊 speak no evil|🐔 chicken|🐧 penguin|🐦 bird|🕊️ dove pigeon peace|🦅 eagle|🦆 duck|🦉 owl|🐝 bee|🦋 butterfly|🐌 snail|🐞 ladybug|🐢 turtle|🐍 snake|🐙 octopus|🐬 dolphin|🐳 whale|🐟 fish|🦈 shark|🐘 elephant|🐎 horse|🌸 cherry blossom|🌹 rose|🌻 sunflower|🌷 tulip|🌱 seedling|🌲 evergreen tree|🌳 tree|🍀 four leaf clover luck|🍁 maple leaf|🌍 earth globe|🌙 moon|☀️ sun|⛅ partly cloudy|🌧️ rain|⛈️ storm|❄️ snowflake|🌈 rainbow|⚡ lightning|🌊 wave water"],
    ["Food", "🍕", "🍎 apple|🍊 orange|🍋 lemon|🍌 banana|🍉 watermelon|🍇 grapes|🍓 strawberry|🍒 cherries|🍑 peach|🥭 mango|🍍 pineapple|🥝 kiwi|🍅 tomato|🥑 avocado|🥕 carrot|🌽 corn|🌶️ hot pepper|🥒 cucumber|🥦 broccoli|🍞 bread|🧀 cheese|🥚 egg|🍳 cooking fried egg|🥞 pancakes|🥓 bacon|🍔 hamburger|🍟 fries|🍕 pizza|🌭 hot dog|🌮 taco|🌯 burrito|🥗 salad|🍝 spaghetti pasta|🍜 ramen noodles|🍲 stew pot|🍣 sushi|🍚 rice|🍰 cake slice|🎂 birthday cake|🧁 cupcake|🍫 chocolate|🍬 candy|🍩 doughnut|🍪 cookie|🍦 ice cream|☕ coffee|🍵 tea|🥛 milk|🍺 beer|🍻 cheers beers|🍷 wine|🥂 clinking glasses toast|🍸 cocktail"],
    ["Travel", "✈️", "🚗 car|🚕 taxi|🚌 bus|🚑 ambulance|🚒 fire engine|🚓 police car|🚲 bicycle|🛵 scooter|🏍️ motorcycle|🚆 train|🚇 metro|✈️ airplane flight|🚀 rocket launch|🚁 helicopter|⛵ sailboat|🚢 ship|🏠 house home|🏢 office building|🏥 hospital|🏫 school|🏖️ beach|🏔️ mountain snow|🏕️ camping|🗺️ map|🧭 compass|⏰ alarm clock|⌛ hourglass|📍 pin location|🚧 construction|🚦 traffic light|⚽ soccer football|🏀 basketball|🎾 tennis|🏐 volleyball|🎮 video game|🎲 dice|🎯 target bullseye|🎬 clapper film|🎤 microphone|🎧 headphones|🎵 music note|🎶 music notes|🎸 guitar|🎹 piano|🎨 art palette"],
    ["Objects", "💡", "💡 light bulb idea|📱 phone mobile|💻 laptop|🖥️ desktop computer|⌨️ keyboard|🖱️ mouse computer|📷 camera|📹 video camera|📺 television|🔋 battery|🔌 plug|💾 floppy save|📁 folder|📂 open folder|📄 document page|📎 paperclip attach|📌 pushpin|✂️ scissors|🖊️ pen|✏️ pencil|📝 memo note|📚 books|📖 open book|📅 calendar|📈 chart up|📉 chart down|📊 bar chart|💰 money bag|💵 dollar|💳 credit card|🧾 receipt|✉️ envelope letter|📨 incoming envelope|📬 mailbox|📦 package box|🔒 lock|🔓 unlock|🔑 key|🛡️ shield|🔨 hammer|🔧 wrench|⚙️ gear settings|🧪 test tube|🔬 microscope|🧹 broom|🛒 shopping cart|👕 shirt|👗 dress|👟 sneaker|🎒 backpack|👓 glasses|💍 ring|💎 gem diamond|🔔 bell|📣 megaphone|🔍 magnifying search"],
    ["Symbols", "✅", "✅ check done yes|☑️ ballot check|✔️ check mark|❌ cross no|❎ cross button|❗ exclamation|❓ question|⁉️ interrobang|‼️ double exclamation|⚠️ warning|🚫 prohibited|⛔ no entry|🆗 ok button|🆕 new|🆒 cool button|🆓 free|➕ plus|➖ minus|➗ divide|✖️ multiply|♾️ infinity|♻️ recycle|➡️ right arrow|⬅️ left arrow|⬆️ up arrow|⬇️ down arrow|↔️ left right arrow|🔄 refresh arrows|🔁 repeat|▶️ play|⏸️ pause|⏹️ stop|⏭️ next track|🔴 red circle|🟠 orange circle|🟡 yellow circle|🟢 green circle|🔵 blue circle|🟣 purple circle|⚫ black circle|⚪ white circle|💬 speech bubble|💭 thought bubble|™️ trademark|©️ copyright|®️ registered"],
    ["Flags", "🏳️", "🏁 checkered flag finish|🚩 red flag|🏳️ white flag|🏴 black flag|🏳️‍🌈 rainbow flag pride|🇹🇷 turkey turkiye|🇺🇸 united states usa|🇬🇧 united kingdom uk|🇪🇺 european union eu|🇩🇪 germany|🇫🇷 france|🇪🇸 spain|🇮🇹 italy|🇳🇱 netherlands|🇯🇵 japan|🇰🇷 south korea|🇨🇳 china|🇮🇳 india|🇧🇷 brazil|🇨🇦 canada|🇦🇺 australia|🇺🇦 ukraine"],
  ].map(([title, icon, list]) => ({
    title,
    icon,
    items: list.split("|").map((entry) => {
      const space = entry.indexOf(" ");
      return { char: entry.slice(0, space), words: entry.slice(space + 1) };
    }),
  }));

  const RECENT_KEY = "pp.emoji.recent";
  const RECENT_MAX = 24;
  const LABELS = new Map(SETS.flatMap((set) => set.items.map((item) => [item.char, item.words])));

  // Storage can be missing or refused (a private window, cleared site data); the picker still works.
  function readRecent() {
    try {
      const list = JSON.parse(localStorage.getItem(RECENT_KEY) || "[]");
      return Array.isArray(list) ? list.filter((c) => LABELS.has(c)).slice(0, RECENT_MAX) : [];
    } catch (_) { return []; }
  }
  function remember(char) {
    const list = [char, ...readRecent().filter((c) => c !== char)].slice(0, RECENT_MAX);
    try { localStorage.setItem(RECENT_KEY, JSON.stringify(list)); } catch (_) { /* kept for this visit only by the open panel */ }
    return list;
  }

  function node(tag, props, ...children) {
    const el = document.createElement(tag);
    for (const [k, v] of Object.entries(props || {})) {
      if (k === "class") el.className = v;
      else if (k === "text") el.textContent = v;
      else if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
      else el.setAttribute(k, v);
    }
    el.append(...children);
    return el;
  }

  function faceIcon() {
    const NS = "http://www.w3.org/2000/svg";
    const svg = document.createElementNS(NS, "svg");
    for (const [k, v] of Object.entries({ viewBox: "0 0 24 24", fill: "none", stroke: "currentColor", "stroke-width": "1.8", "stroke-linecap": "round", "stroke-linejoin": "round", "aria-hidden": "true" })) svg.setAttribute(k, v);
    const circle = document.createElementNS(NS, "circle");
    circle.setAttribute("cx", "12"); circle.setAttribute("cy", "12"); circle.setAttribute("r", "9");
    const mouth = document.createElementNS(NS, "path");
    mouth.setAttribute("d", "M8.5 14.5s1.3 2 3.5 2 3.5-2 3.5-2");
    const eyes = document.createElementNS(NS, "path");
    eyes.setAttribute("d", "M9 9.5h.01M15 9.5h.01"); eyes.setAttribute("stroke-width", "2.6");
    svg.append(circle, mouth, eyes);
    return svg;
  }

  // Puts `char` where the caret is, replacing any selection, and tells the composer through the
  // same input event typing raises, so its height, send button and draft all follow.
  function insertAtCaret(box, char) {
    if (box.disabled) return false;
    const start = box.selectionStart ?? box.value.length;
    const end = box.selectionEnd ?? start;
    box.setRangeText(char, start, end, "end");
    box.dispatchEvent(new Event("input", { bubbles: true }));
    return true;
  }

  function attach(box) {
    if (!box || box.dataset.emoji) return null;
    box.dataset.emoji = "1";

    let panel = null;
    let body = null;
    let search = null;
    let recentSection = null;
    let cells = [];
    let isOpen = false;

    const button = node("button", {
      class: "emoji-btn", type: "button", "aria-label": "Emoji", "aria-haspopup": "dialog", "aria-expanded": "false",
      onclick: () => (isOpen ? close(false) : open()),
    }, faceIcon());
    box.after(button);

    function cell(char) {
      return node("button", {
        class: "emoji-cell", type: "button", tabindex: "-1", role: "gridcell", "aria-label": LABELS.get(char).split(" ")[0], "data-words": LABELS.get(char), text: char,
        onclick: (e) => choose(char, e),
      });
    }

    function section(title, chars, extra) {
      return node("section", extra || {},
        node("h3", { class: "emoji-heading", text: title }),
        node("div", { class: "emoji-grid", role: "grid", "aria-label": title }, ...chars.map(cell)));
    }

    function drawRecent(list) {
      const fresh = list.length ? section("Recent", list, { "data-recent": "1" }) : node("section", { "data-recent": "1", hidden: "" });
      recentSection.replaceWith(fresh);
      recentSection = fresh;
    }

    // What the arrow keys walk: every emoji still showing, in reading order. One of them is in the
    // Tab order, so the panel is a single stop instead of several hundred.
    function refreshCells() {
      cells = [...body.querySelectorAll(".emoji-cell")].filter((c) => !c.hidden && !c.closest("[hidden]"));
      cells.forEach((c, i) => c.setAttribute("tabindex", i === 0 ? "0" : "-1"));
    }

    function applyFilter() {
      const q = search.value.trim().toLowerCase();
      body.querySelectorAll("section").forEach((sec) => {
        if (sec.dataset.recent && q) { sec.hidden = true; return; }
        if (sec.dataset.recent) { sec.hidden = !sec.querySelector(".emoji-cell"); return; }
        let any = false;
        sec.querySelectorAll(".emoji-cell").forEach((c) => {
          c.hidden = Boolean(q) && !c.dataset.words.includes(q);
          any = any || !c.hidden;
        });
        sec.hidden = !any;
      });
      refreshCells();
      body.querySelector(".emoji-none").hidden = cells.length > 0;
    }

    function choose(char, e) {
      if (!insertAtCaret(box, char)) return;
      const list = remember(char);
      if (!search.value.trim()) {
        const keep = body.scrollTop;
        const focusedRecent = recentSection.contains(document.activeElement);
        drawRecent(list);
        applyFilter();
        body.scrollTop = keep;
        if (focusedRecent) recentSection.querySelector(".emoji-cell")?.focus({ preventScroll: true });
      }
      // A mouse click goes back to typing. A keyboard keeps its place, and a touch screen keeps its
      // on-screen keyboard closed so the panel stays in view.
      if (e && e.detail > 0 && !window.matchMedia?.("(hover: none)")?.matches) box.focus({ preventScroll: true });
    }

    function columns() {
      if (cells.length < 2) return 1;
      const top = cells[0].offsetTop;
      const n = cells.findIndex((c) => c.offsetTop !== top);
      return n > 0 ? n : 8;
    }

    function moveFrom(index, delta) {
      const next = Math.max(0, Math.min(cells.length - 1, index + delta));
      cells.forEach((c, i) => c.setAttribute("tabindex", i === next ? "0" : "-1"));
      cells[next]?.focus();
      cells[next]?.scrollIntoView?.({ block: "nearest" });
    }

    function onGridKey(e) {
      const at = cells.indexOf(document.activeElement);
      if (at < 0) return;
      const cols = columns();
      const delta = { ArrowLeft: -1, ArrowRight: 1, ArrowUp: -cols, ArrowDown: cols }[e.key];
      if (delta !== undefined) {
        e.preventDefault();
        if (e.key === "ArrowUp" && at < cols) { search.focus(); return; }
        moveFrom(at, delta);
      } else if (e.key === "Home") { e.preventDefault(); moveFrom(0, 0); }
      else if (e.key === "End") { e.preventDefault(); moveFrom(cells.length - 1, 0); }
    }

    function build() {
      search = node("input", {
        class: "emoji-search", type: "search", placeholder: "Search emoji", "aria-label": "Search emoji", autocomplete: "off",
        oninput: applyFilter,
        onkeydown: (e) => {
          // The panel sits inside the composer's form: Enter here must never send the message.
          if (e.key === "Enter") { e.preventDefault(); if (cells[0]) choose(cells[0].textContent); }
          else if (e.key === "ArrowDown" && cells[0]) { e.preventDefault(); moveFrom(0, 0); }
        },
      });
      recentSection = node("section", { "data-recent": "1", hidden: "" });
      const sections = SETS.map((set) => section(set.title, set.items.map((i) => i.char)));
      body = node("div", { class: "emoji-body", onkeydown: onGridKey }, recentSection, ...sections, node("p", { class: "emoji-none", text: "No emoji found", hidden: "" }));
      const tabs = node("div", { class: "emoji-tabs" }, ...SETS.map((set, i) => node("button", {
        class: "emoji-tab", type: "button", "aria-label": set.title, title: set.title, text: set.icon,
        onclick: () => {
          if (search.value) { search.value = ""; applyFilter(); }
          body.scrollTop = sections[i].offsetTop - body.offsetTop;
        },
      })));
      panel = node("div", { class: "emoji-panel", role: "dialog", "aria-label": "Emoji", hidden: "" }, search, tabs, body);
      button.after(panel);
    }

    function onOutside(e) {
      if (!panel.contains(e.target) && !button.contains(e.target)) close(false);
    }
    function onEscape(e) {
      if (e.key !== "Escape") return;
      // Caught first: in a reply or an edit, Escape on the box would otherwise cancel that too.
      e.preventDefault();
      e.stopPropagation();
      close(true);
    }
    function onSubmit() { close(false); }

    function open() {
      if (!panel) build();
      isOpen = true;
      search.value = "";
      drawRecent(readRecent());
      panel.hidden = false;
      button.setAttribute("aria-expanded", "true");
      applyFilter();
      body.scrollTop = 0;
      document.addEventListener("pointerdown", onOutside, true);
      document.addEventListener("keydown", onEscape, true);
      box.form?.addEventListener("submit", onSubmit);
      if (!window.matchMedia?.("(hover: none)")?.matches) search.focus({ preventScroll: true });
    }

    function close(refocus) {
      if (!isOpen) return;
      isOpen = false;
      panel.hidden = true;
      button.setAttribute("aria-expanded", "false");
      document.removeEventListener("pointerdown", onOutside, true);
      document.removeEventListener("keydown", onEscape, true);
      box.form?.removeEventListener("submit", onSubmit);
      if (refocus) button.focus({ preventScroll: true });
    }

    return { button, open, close };
  }

  window.PigeonpostEmoji = { attach, sets: SETS };
})();
