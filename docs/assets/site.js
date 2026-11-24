// mcc — small progressive enhancements. The site works without this file.
(function () {
  "use strict";

  // Copy buttons on code blocks. Prompt markers (.p) are not copied.
  document.querySelectorAll(".code").forEach(function (block) {
    var pre = block.querySelector("pre");
    if (!pre || !navigator.clipboard) return;
    var btn = document.createElement("button");
    btn.type = "button";
    btn.className = "copy";
    btn.textContent = "Copy";
    btn.setAttribute("aria-label", "Copy code to clipboard");
    btn.addEventListener("click", function () {
      var clone = pre.cloneNode(true);
      clone.querySelectorAll(".p").forEach(function (n) { n.remove(); });
      navigator.clipboard.writeText(clone.textContent.replace(/\n+$/, "")).then(function () {
        btn.textContent = "Copied";
        btn.setAttribute("data-copied", "");
        setTimeout(function () {
          btn.textContent = "Copy";
          btn.removeAttribute("data-copied");
        }, 1600);
      }, function () {
        btn.textContent = "Press ⌘C";
      });
    });
    block.appendChild(btn);
  });

  // Highlight the table-of-contents entry for the section in view.
  var links = Array.prototype.slice.call(document.querySelectorAll(".toc a[href^='#']"));
  if (!links.length || !("IntersectionObserver" in window)) return;
  var byId = {};
  links.forEach(function (a) { byId[a.getAttribute("href").slice(1)] = a; });
  var targets = Object.keys(byId)
    .map(function (id) { return document.getElementById(id); })
    .filter(Boolean);
  var visible = {};
  var observer = new IntersectionObserver(function (entries) {
    entries.forEach(function (e) { visible[e.target.id] = e.isIntersecting; });
    var current = null;
    for (var i = 0; i < targets.length; i++) {
      if (visible[targets[i].id]) { current = targets[i].id; break; }
    }
    if (!current) return;
    links.forEach(function (a) { a.classList.remove("active"); });
    byId[current].classList.add("active");
  }, { rootMargin: "-80px 0px -65% 0px", threshold: 0 });
  targets.forEach(function (t) { observer.observe(t); });
})();

// Terminal simulator on the home page: one tab per feature. Each panel holds
// a static transcript in a <pre>, which is what screen readers, no-JS and
// reduced-motion visitors get. Here the selected tab's transcript is
// replayed over it: each "$ " line is typed out, then the lines after it
// appear one by one. A replay runs once, pauses off-screen and in background
// tabs, and can be skipped or replayed.
(function () {
  "use strict";

  var sim = document.querySelector("[data-sim]");
  if (!sim) return;
  var strip = sim.querySelector("[role='tablist']");
  var tabs = Array.prototype.slice.call(sim.querySelectorAll("[role='tab']"));
  var panels = tabs.map(function (tab) {
    return document.getElementById(tab.getAttribute("aria-controls"));
  });
  if (!strip || !tabs.length || panels.indexOf(null) !== -1) return;

  var motion = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)");
  function still() { return !!(motion && motion.matches); }

  var PROMPT = '<span class="p">$ </span>';
  var CARET = '<span class="sim-caret"></span>';
  function esc(s) { return s.replace(/&/g, "&amp;").replace(/</g, "&lt;"); }

  // One entry per transcript line: a command to type, or output to show.
  // A command keeps the pieces after its prompt (command, comment) so it is
  // typed in their colors.
  function parse(pre) {
    return pre.innerHTML.replace(/\n+$/, "").split("\n").map(function (html) {
      var box = document.createElement("span");
      box.innerHTML = html;
      var cmd = html.indexOf(PROMPT) === 0;
      var parts = cmd ? Array.prototype.slice.call(box.childNodes, 1).map(function (node) {
        return { cls: node.nodeType === 1 ? node.className : "", text: node.textContent };
      }) : [];
      return { html: html, cmd: cmd, parts: parts, length: box.textContent.length - (cmd ? 2 : 0) };
    });
  }

  // The first n characters typed of a command line, in its colors.
  function typed(line, n) {
    return line.parts.map(function (part) {
      var text = esc(part.text.slice(0, Math.max(n, 0)));
      n -= part.text.length;
      return part.cls && text ? '<span class="' + part.cls + '">' + text + "</span>" : text;
    }).join("");
  }

  // Replays one panel's transcript. `onState` hears "playing" and "done".
  function createPlayer(screen, onState) {
    var lines = parse(screen.querySelector("pre"));
    var play = document.createElement("pre");
    play.className = "sim-play";
    play.setAttribute("aria-hidden", "true");
    screen.appendChild(play);
    screen.classList.add("is-playing");

    var shown = ""; // finished lines, as HTML
    var read = 0;   // characters of output since the last command
    // A single timer chain: `later` queues the next step, which waits while
    // the replay is paused and runs once it resumes.
    var timer = null, next = null, running = false;

    function draw(tail) {
      play.innerHTML = shown + tail;
      screen.scrollTop = screen.scrollHeight; // follow the output, as a terminal does
    }
    function later(fn, ms) {
      next = fn;
      if (running) timer = setTimeout(tick, ms);
    }
    function tick() {
      var fn = next;
      timer = next = null;
      fn();
    }
    function clear() {
      if (timer) clearTimeout(timer);
      timer = next = null;
    }
    function setRunning(on) {
      if (on === running) return;
      running = on;
      if (on && next) {
        timer = setTimeout(tick, 300);
      } else if (!on && timer) {
        clearTimeout(timer);
        timer = null;
      }
    }

    function step(i) {
      if (i === lines.length) {
        draw(PROMPT + CARET); // an idle prompt: the replay is over
        return onState("done");
      }
      var line = lines[i];
      if (!line.cmd) {
        shown += line.html + "\n";
        read += line.length;
        draw("");
        return later(function () { step(i + 1); }, 70);
      }
      // A prompt, a pause long enough to glance at the output above, then typing.
      draw(PROMPT + CARET);
      screen.scrollLeft = 0;
      var pause = i ? Math.min(2600, 800 + read * 4) : 500;
      read = 0;
      later(function () { type(i, 1); }, pause);
    }

    function type(i, n) {
      var line = lines[i];
      draw(PROMPT + typed(line, n) + CARET);
      if (n < line.length) {
        return later(function () { type(i, n + 1); }, 26 + Math.random() * 38);
      }
      later(function () {
        shown += line.html + "\n";
        draw("");
        later(function () { step(i + 1); }, 320);
      }, 380);
    }

    return {
      setRunning: setRunning,
      start: function () {
        clear();
        shown = "";
        read = 0;
        screen.scrollLeft = 0;
        onState("playing");
        step(0);
      },
      // Skip: the whole transcript at once, from the top.
      finish: function () {
        clear();
        shown = lines.map(function (line) { return line.html; }).join("\n") + "\n";
        play.innerHTML = shown + PROMPT + CARET;
        screen.scrollTop = screen.scrollLeft = 0;
        onState("done");
      },
      destroy: function () {
        clear();
        play.remove();
        screen.classList.remove("is-playing");
      }
    };
  }

  // Each terminal gets a bar: Skip while its replay runs, Replay after, and
  // a button to the next tab.
  function button(cls, text) {
    var b = document.createElement("button");
    b.type = "button";
    b.className = "sim-btn " + cls;
    b.textContent = text;
    return b;
  }
  var players = [], states = [], againButtons = [];
  panels.forEach(function (panel, i) {
    var bar = document.createElement("div");
    bar.className = "sim-bar";
    var again = button("sim-again", "Skip");
    again.hidden = true;
    again.addEventListener("click", function () {
      if (!players[i]) return;
      if (states[i] === "playing") players[i].finish();
      else players[i].start();
    });
    var after = panels[(i + 1) % panels.length];
    var nextTab = button("sim-next", "Next: " + after.getAttribute("data-label") + " →");
    nextTab.addEventListener("click", function () { select((i + 1) % panels.length, true); });
    bar.appendChild(again);
    bar.appendChild(nextTab);
    panel.querySelector(".term").appendChild(bar);
    againButtons[i] = again;
  });

  function setState(i, state) {
    states[i] = state;
    againButtons[i].hidden = false;
    againButtons[i].textContent = state === "playing" ? "Skip" : "Replay";
  }

  var current = -1;
  var onScreen = !("IntersectionObserver" in window);
  function update() {
    if (players[current]) players[current].setRunning(onScreen && !document.hidden);
  }

  function select(i, focus) {
    if (i !== current) {
      if (players[current]) players[current].setRunning(false);
      tabs.forEach(function (tab, j) {
        tab.setAttribute("aria-selected", j === i ? "true" : "false");
        tab.tabIndex = j === i ? 0 : -1;
        panels[j].hidden = j !== i;
      });
      current = i;
      // Keep the selected tab in view in the strip without moving the page.
      strip.scrollLeft = tabs[i].offsetLeft - (strip.clientWidth - tabs[i].offsetWidth) / 2;
      if (!still()) {
        if (!players[i]) {
          players[i] = createPlayer(panels[i].querySelector(".sim-screen"), function (state) {
            setState(i, state);
          });
        }
        players[i].start();
        update();
      }
    }
    if (focus) tabs[i].focus({ preventScroll: true });
  }

  tabs.forEach(function (tab, i) {
    tab.addEventListener("click", function () { select(i, false); });
    tab.addEventListener("keydown", function (e) {
      var to = { ArrowRight: i + 1, ArrowLeft: i - 1, Home: 0, End: tabs.length - 1 }[e.key];
      if (to === undefined) return;
      e.preventDefault();
      select((to + tabs.length) % tabs.length, true);
    });
  });

  // Links to a panel (the hero's example setup) open its tab.
  function panelOf(hash) {
    var id = hash.charAt(0) === "#" ? hash.slice(1) : "";
    for (var i = 0; i < panels.length; i++) if (id && panels[i].id === id) return i;
    return -1;
  }
  function open(i) {
    select(i, false);
    sim.scrollIntoView({ behavior: still() ? "auto" : "smooth", block: "start" });
  }
  document.addEventListener("click", function (e) {
    var a = e.target.closest ? e.target.closest("a[href^='#sim-']") : null;
    var i = a ? panelOf(a.getAttribute("href")) : -1;
    if (i === -1) return;
    e.preventDefault();
    open(i);
  });
  window.addEventListener("hashchange", function () {
    var i = panelOf(location.hash);
    if (i !== -1) open(i);
  });

  if (!onScreen) {
    new IntersectionObserver(function (entries) {
      onScreen = entries[entries.length - 1].isIntersecting;
      update();
    }, { threshold: 0.2 }).observe(sim);
  }
  document.addEventListener("visibilitychange", update);

  // Reduced motion turned on or off while the page is open: back to the
  // static transcripts, or replay the selected tab.
  function motionChanged() {
    players.forEach(function (player) { if (player) player.destroy(); });
    players = [];
    againButtons.forEach(function (b) { b.hidden = true; });
    if (!still()) {
      var i = current;
      current = -1;
      select(i, false);
    }
  }
  if (motion && motion.addEventListener) motion.addEventListener("change", motionChanged);
  else if (motion && motion.addListener) motion.addListener(motionChanged);

  sim.classList.add("is-live");
  var first = panelOf(location.hash);
  select(first === -1 ? 0 : first, false);
})();
