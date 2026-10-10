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
