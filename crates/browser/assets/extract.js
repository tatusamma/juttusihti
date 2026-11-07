// Runs in the loaded page right after Readability.js (Mozilla Readability 0.6.0, Apache-2.0,
// vendored verbatim from https://raw.githubusercontent.com/mozilla/readability/0.6.0/Readability.js).
// The completion value is a JSON string that engine.rs parses into `Extracted`.
(function () {
  function absolute(url) {
    try {
      return new URL(url, document.baseURI).href;
    } catch (e) {
      return null;
    }
  }

  // Every JSON-LD object on the page, flattening arrays and @graph lists.
  function jsonLdObjects() {
    var found = [];
    function walk(value) {
      if (Array.isArray(value)) {
        value.forEach(walk);
      } else if (value && typeof value === "object") {
        found.push(value);
        if (value["@graph"]) walk(value["@graph"]);
      }
    }
    document.querySelectorAll('script[type="application/ld+json"]').forEach(function (script) {
      try {
        walk(JSON.parse(script.textContent));
      } catch (e) {
        // Broken JSON-LD is common; ignore it.
      }
    });
    return found;
  }

  function isArticleObject(o) {
    var types = Array.isArray(o["@type"]) ? o["@type"] : [o["@type"]];
    return types.some(function (t) {
      return typeof t === "string" && /Article$/.test(t);
    });
  }

  // A generic share picture or logo rather than the article's own picture. IS and HS keep their
  // share defaults under /some/ (Finnish for social media): some/is-default.jpg, some/hs-visio.jpg.
  function isSiteDefault(url) {
    var path;
    try {
      path = new URL(url, document.baseURI).pathname.toLowerCase();
    } catch (e) {
      return true;
    }
    if (/\/some\//.test(path)) return true;
    var file = path.split("/").pop();
    return /(^|[^a-z])(default|logo|placeholder|fallback|favicon|icon|sprite|avatar|blank|spacer|share)([^a-z]|$)/.test(file);
  }

  // Formats the app cannot decode (it keeps JPEG, PNG, WebP and GIF).
  function undecodable(url) {
    try {
      return /\.(avif|svg|heic|jxl)$/.test(new URL(url, document.baseURI).pathname.toLowerCase());
    } catch (e) {
      return true;
    }
  }

  // Only web pictures: never file:, data:, blob: or any other scheme a page could point at.
  function usable(url) {
    return !!url && /^https?:\/\//i.test(url) && !undecodable(url) && !isSiteDefault(url);
  }

  // The best candidate of a srcset: the widest up to 2000 px, else the narrowest wider one.
  function fromSrcset(srcset) {
    if (!srcset) return null;
    var best = null;
    var bestWidth = 0;
    var smallestOver = null;
    var smallestOverWidth = Infinity;
    srcset.split(",").forEach(function (part) {
      var bits = part.trim().split(/\s+/);
      if (!bits[0]) return;
      var width = 0;
      if (bits[1] && /w$/.test(bits[1])) width = parseInt(bits[1], 10) || 0;
      if (width <= 2000 && width >= bestWidth) {
        best = bits[0];
        bestWidth = width;
      } else if (width > 2000 && width < smallestOverWidth) {
        smallestOver = bits[0];
        smallestOverWidth = width;
      }
    });
    return best || smallestOver;
  }

  // Known pixel width of an image in the article: its attributes, or the loaded copy in the live page.
  function knownWidth(img, url) {
    var attr = parseInt(img.getAttribute("width"), 10);
    if (attr > 0) return attr;
    var live = document.images;
    for (var i = 0; i < live.length; i++) {
      if (live[i].currentSrc === url || live[i].src === url) {
        if (live[i].naturalWidth > 0) return live[i].naturalWidth;
      }
    }
    return 0;
  }

  // The first large picture inside the article Readability found.
  function leadImage(content) {
    var images = content.querySelectorAll("img");
    for (var i = 0; i < images.length; i++) {
      var img = images[i];
      // The <img> itself first (its src is the widely supported fallback), then <picture> sources.
      var raws = [fromSrcset(img.getAttribute("srcset")), img.getAttribute("src"), img.getAttribute("data-src")];
      var picture = img.closest("picture");
      if (picture) {
        picture.querySelectorAll("source[srcset]").forEach(function (source) {
          raws.push(fromSrcset(source.getAttribute("srcset")));
        });
      }
      for (var j = 0; j < raws.length; j++) {
        var url = raws[j] ? absolute(raws[j]) : null;
        if (!usable(url)) continue;
        var width = knownWidth(img, url);
        if (width === 0 || width >= 400) return url;
        break;
      }
    }
    return null;
  }

  function jsonLdImage(objects) {
    for (var i = 0; i < objects.length; i++) {
      if (!isArticleObject(objects[i])) continue;
      var image = objects[i].image;
      if (Array.isArray(image)) image = image[0];
      if (image && typeof image === "object") image = image.url || image.contentUrl;
      var url = typeof image === "string" ? absolute(image) : null;
      if (usable(url)) return url;
    }
    return null;
  }

  function ogImage() {
    var og = document.querySelector('meta[property="og:image"]');
    var url = og && og.getAttribute("content") ? absolute(og.getAttribute("content")) : null;
    return usable(url) ? url : null;
  }

  // Block elements end a paragraph; everything else flows into the current one.
  var BLOCKS = /^(P|DIV|SECTION|ARTICLE|MAIN|HEADER|FOOTER|ASIDE|BLOCKQUOTE|PRE|UL|OL|LI|DL|DT|DD|H[1-6]|FIGURE|FIGCAPTION|TABLE|THEAD|TBODY|TR|TD|TH|HR|ADDRESS|DETAILS|SUMMARY)$/;
  var SKIP = /^(SCRIPT|STYLE|NOSCRIPT|TEMPLATE|BUTTON|SVG|FORM|NAV|IFRAME)$/;

  // Paragraphs separated by blank lines; engine.rs normalizes the whitespace.
  function paragraphs(content) {
    var out = [];
    var current = "";
    function flush() {
      if (current.trim()) out.push(current.replace(/[ \t\r\n ]+/g, " ").trim());
      current = "";
    }
    function walk(node) {
      if (node.nodeType === 3) {
        current += node.nodeValue;
        return;
      }
      if (node.nodeType !== 1) return;
      var tag = node.tagName.toUpperCase();
      if (SKIP.test(tag) || node.getAttribute("aria-hidden") === "true" || node.hidden) return;
      // A link wrapping blocks is a teaser card for another story, not part of this one.
      if (tag === "A" && node.querySelector("p, div, li, figure, h1, h2, h3, h4, h5, h6")) return;
      if (tag === "BR") {
        current += " ";
        return;
      }
      var block = BLOCKS.test(tag);
      if (block) flush();
      for (var child = node.firstChild; child; child = child.nextSibling) walk(child);
      if (block) flush();
    }
    walk(content);
    flush();
    return out.join("\n\n");
  }

  function notFree(value) {
    return value === false || (typeof value === "string" && value.trim().toLowerCase() === "false");
  }

  // The parts a JSON-LD object marks as not free (schema.org paywalled content markup).
  function paidParts(objects) {
    var parts = [];
    objects.forEach(function (o) {
      var list = Array.isArray(o.hasPart) ? o.hasPart : [o.hasPart];
      list.forEach(function (part) {
        if (part && typeof part === "object" && notFree(part.isAccessibleForFree)) parts.push(part);
      });
    });
    return parts;
  }

  // Never read what the reader of the page cannot see. Elements are marked in the live page and
  // removed from the copy Readability parses: the copy has no computed style of its own.
  var DROP = "data-juttusihti-drop";
  // Elements that are display:none by default or carry no text of their own.
  var NO_STYLE_CHECK = /^(SCRIPT|STYLE|NOSCRIPT|TEMPLATE|SOURCE|TRACK|LINK|META|BASE|TITLE|PARAM|DATALIST|BR|WBR)$/;

  // Marks the elements the paid parts name and returns how many of them hold text. Sites often
  // declare a paid part on every article, free ones too, with a selector that matches nothing.
  function markPaidParts(parts) {
    var withText = 0;
    parts.forEach(function (part) {
      var selectors = Array.isArray(part.cssSelector) ? part.cssSelector : [part.cssSelector];
      selectors.forEach(function (selector) {
        if (typeof selector !== "string" || !selector.trim()) return;
        try {
          document.querySelectorAll(selector).forEach(function (el) {
            el.setAttribute(DROP, "");
            if ((el.textContent || "").trim()) withText++;
          });
        } catch (e) {
          // An invalid selector marks nothing.
        }
      });
    });
    return withText;
  }

  function hidden(el) {
    try {
      var style = getComputedStyle(el);
      return style.display === "none" || style.visibility === "hidden";
    } catch (e) {
      return false;
    }
  }

  function markHidden() {
    // A page that hides itself until its scripts finish (anti-flicker) would lose everything.
    if (!document.body || hidden(document.documentElement) || hidden(document.body)) return;
    function walk(el) {
      for (var child = el.firstElementChild; child; child = child.nextElementSibling) {
        if (child.hasAttribute(DROP)) continue;
        var tag = child.tagName.toUpperCase();
        if (NO_STYLE_CHECK.test(tag)) continue;
        if (hidden(child)) {
          child.setAttribute(DROP, "");
          continue;
        }
        // SVG and MathML hold no article text.
        if (child.namespaceURI === "http://www.w3.org/1999/xhtml") walk(child);
      }
    }
    walk(document.body);
  }

  var objects = jsonLdObjects();
  var paid = paidParts(objects);
  // Paywalled when the page says the article itself is not free, or when a part it marks as paid
  // is really on the page. A declared paid part that matches nothing does not count.
  var paywalled = objects.some(function (o) {
    return notFree(o.isAccessibleForFree);
  });

  var article = null;
  try {
    if (markPaidParts(paid) > 0) paywalled = true;
    markHidden();
    var copy = document.cloneNode(true);
    copy.querySelectorAll("[" + DROP + "]").forEach(function (el) {
      el.remove();
    });
    article = new Readability(copy).parse();
  } catch (e) {
    article = null;
  }

  // An inert copy of the article: nothing in it loads or runs.
  var content = null;
  if (article && article.content) {
    content = new DOMParser().parseFromString(article.content, "text/html").body;
  }

  var picture = (content && leadImage(content)) || jsonLdImage(objects) || ogImage();

  // Servo 0.7 does not expose the main document's HTTP status to the embedder (resource-load
  // hooks see requests only) and has no navigation-timing responseStatus; kept for engines that do.
  var status = null;
  try {
    var nav = performance.getEntriesByType("navigation")[0];
    if (nav && typeof nav.responseStatus === "number" && nav.responseStatus > 0) {
      status = nav.responseStatus;
    }
  } catch (e) {
    status = null;
  }

  return JSON.stringify({
    main_status: status,
    title: (article && article.title) || document.title || "",
    text: content ? paragraphs(content) : "",
    picture_url: picture,
    paywalled: paywalled,
  });
})();
