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

// Hero terminal tour. The static transcript in the hero's <pre> is replayed:
// each "$ " line is typed out, then the lines after it appear one by one.
// The <pre> keeps its place, so the hero never changes height, and it stays
// what screen readers get; the replay is drawn over it, hidden from them.
// Skipped with reduced motion; paused in background tabs and off-screen.
(function () {
  "use strict";

  var screen = document.querySelector("[data-tour] .tour-screen");
  var pre = screen && screen.querySelector("pre");
  var motion = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)");
  if (!pre || (motion && motion.matches)) return;

  // One entry per transcript line: a command to type, or output to show.
  // A command keeps the pieces after its prompt (command, comment) so it is
  // typed in their colors.
  var PROMPT = '<span class="p">$ </span>';
  var CARET = '<span class="tour-caret"></span>';
  var lines = pre.innerHTML.replace(/\n+$/, "").split("\n").map(function (html) {
    var box = document.createElement("span");
    box.innerHTML = html;
    var cmd = html.indexOf(PROMPT) === 0;
    var parts = cmd ? Array.prototype.slice.call(box.childNodes, 1).map(function (node) {
      return { cls: node.nodeType === 1 ? node.className : "", text: node.textContent };
    }) : [];
    return { html: html, cmd: cmd, parts: parts, length: box.textContent.length - (cmd ? 2 : 0) };
  });

  var play = document.createElement("pre");
  play.className = "tour-play";
  play.setAttribute("aria-hidden", "true");
  screen.appendChild(play);
  screen.classList.add("is-playing");

  var shown = ""; // finished lines, as HTML
  var read = 0;   // characters of output since the last command
  function draw(tail) { play.innerHTML = shown + tail; }
  function esc(s) { return s.replace(/&/g, "&amp;").replace(/</g, "&lt;"); }

  // The first n characters typed of a command line, in its colors.
  function typed(line, n) {
    return line.parts.map(function (part) {
      var text = esc(part.text.slice(0, Math.max(n, 0)));
      n -= part.text.length;
      return part.cls && text ? '<span class="' + part.cls + '">' + text + "</span>" : text;
    }).join("");
  }

  // A single timer chain: `later` queues the next step, which waits while
  // the tour is paused and runs once it resumes.
  var timer = null, next = null, running = false, stopped = false;
  function later(fn, ms) {
    next = fn;
    if (running) timer = setTimeout(tick, ms);
  }
  function tick() {
    var fn = next;
    timer = next = null;
    fn();
  }
  function setRunning(on) {
    if (on === running) return;
    running = on;
    if (on && next) {
      timer = setTimeout(tick, 400);
    } else if (!on && timer) {
      clearTimeout(timer);
      timer = null;
    }
  }

  function step(i) {
    if (i === lines.length) {
      // The whole transcript is on screen: hold it, then start over.
      draw("");
      return later(function () { shown = ""; read = 0; step(0); }, 5000);
    }
    var line = lines[i];
    if (!line.cmd) {
      shown += line.html + "\n";
      read += line.length;
      draw("");
      return later(function () { step(i + 1); }, 140);
    }
    // A prompt, a pause long enough to read the output above, then typing.
    draw(PROMPT + CARET);
    var pause = i ? Math.min(4500, 900 + read * 6) : 700;
    read = 0;
    later(function () { type(i, 1); }, pause);
  }

  function type(i, n) {
    var line = lines[i];
    draw(PROMPT + typed(line, n) + CARET);
    if (n < line.length) {
      return later(function () { type(i, n + 1); }, 45 + Math.random() * 60);
    }
    later(function () {
      shown += line.html + "\n";
      draw("");
      later(function () { step(i + 1); }, 350);
    }, 450);
  }

  var onScreen = !("IntersectionObserver" in window);
  function update() { setRunning(!stopped && onScreen && !document.hidden); }
  if (!onScreen) {
    new IntersectionObserver(function (entries) {
      onScreen = entries[entries.length - 1].isIntersecting;
      update();
    }).observe(screen);
  }
  document.addEventListener("visibilitychange", update);

  // Reduced motion turned on while the page is open: back to the static text.
  function stop(e) {
    if (!e.matches) return;
    stopped = true;
    update();
    next = null;
    screen.classList.remove("is-playing");
    play.remove();
  }
  if (motion && motion.addEventListener) motion.addEventListener("change", stop);
  else if (motion && motion.addListener) motion.addListener(stop);

  step(0);
  update();
})();
