"use strict";

window.AllowBackFromHistory = false;
function CheckLocation() {
    var start = "#go_to_message";
    var hash = location.hash;
    if (hash.substr(0, start.length) == start) {
        var messageId = parseInt(hash.substr(start.length));
        if (messageId) {
            GoToMessage(messageId);
        }
    } else if (hash == "#allow_back") {
        window.AllowBackFromHistory = true;
    } else {
        ScrollToBottom();
    }
}

function ScrollToBottom() {
    var html = document.documentElement;
    if (html) {
        html.scrollTop = html.scrollHeight;
    } else if (document.body) {
        window.scrollTo(0, document.body.scrollHeight);
    }
}

function ShowToast(text) {
    var container = document.createElement("div");
    container.className = "toast_container";
    var inner = container.appendChild(document.createElement("div"));
    inner.className = "toast_body";
    inner.appendChild(document.createTextNode(text));
    var appended = document.body.appendChild(container);
    setTimeout(function () {
        AddClass(appended, "toast_shown");
        setTimeout(function () {
            RemoveClass(appended, "toast_shown");
            setTimeout(function () {
                document.body.removeChild(appended);
            }, 3000);
        }, 3000);
    }, 0);
}

function ShowHashtag(tag) {
    ShowToast("This is a hashtag '#" + tag + "' link.");
    return false;
}

function ShowCashtag(tag) {
    ShowToast("This is a cashtag '$" + tag + "' link.");
    return false;
}

function ShowBotCommand(command) {
    ShowToast("This is a bot command '/" + command + "' link.");
    return false;
}

function ShowMentionName() {
    ShowToast("This is a link to a user mentioned by name.");
    return false;
}

function ShowNotLoadedEmoji() {
    ShowToast("This custom emoji is not included, change data exporting settings to download.");
    return false;
}

function ShowNotAvailableEmoji() {
    ShowToast("This custom emoji is not available.");
    return false;
}

function ShowTextCopied(content) {
    navigator.clipboard.writeText(content);
    ShowToast("Text copied to clipboard.");
    return false;
}

function ShowSpoiler(target) {
    if (target.classList.contains("hidden")) {
        target.classList.toggle("hidden");
    }
}

function AddClass(element, name) {
    var current = element.className;
    var expression = new RegExp('(^|\\s)' + name + '(\\s|$)', 'g');
    if (expression.test(current)) {
        return;
    }
    element.className = current + ' ' + name;
}

function RemoveClass(element, name) {
    var current = element.className;
    var expression = new RegExp('(^|\\s)' + name + '(\\s|$)', '');
    var match = expression.exec(current);
    while ((match = expression.exec(current)) != null) {
        if (match[1].length > 0 && match[2].length > 0) {
            current = current.substr(0, match.index + match[1].length)
                + current.substr(match.index + match[0].length);
        } else {
            current = current.substr(0, match.index)
                + current.substr(match.index + match[0].length);
        }
    }
    element.className = current;
}

function EaseOutQuad(t) {
    return t * t;
}

function EaseInOutQuad(t) {
    return (t < 0.5) ? (2 * t * t) : ((4 - 2 * t) * t - 1);
}

function ScrollHeight() {
    if ("innerHeight" in window) {
        return window.innerHeight;
    } else if (document.documentElement) {
        return document.documentElement.clientHeight;
    }
    return document.body.clientHeight;
}

function ScrollTo(top, callback) {
    var html = document.documentElement;
    var current = html.scrollTop;
    var delta = top - current;
    var finish = function () {
        html.scrollTop = top;
        if (callback) {
            callback();
        }
    };
    if (!window.performance.now || delta == 0) {
        finish();
        return;
    }
    var transition = EaseOutQuad;
    var max = 300;
    if (delta < -max) {
        current = top + max;
        delta = -max;
    } else if (delta > max) {
        current = top - max;
        delta = max;
    } else {
        transition = EaseInOutQuad;
    }
    var duration = 150;
    var interval = 7;
    var time = window.performance.now();
    var animate = function () {
        var now = window.performance.now();
        if (now >= time + duration) {
            finish();
            return;
        }
        var dt = (now - time) / duration;
        html.scrollTop = Math.round(current + delta * transition(dt));
        setTimeout(animate, interval);
    };
    setTimeout(animate, interval);
}

function ScrollToElement(element, callback) {
    var header = document.getElementsByClassName("page_header")[0];
    var headerHeight = header.offsetHeight;
    var html = document.documentElement;
    var scrollHeight = ScrollHeight();
    var available = scrollHeight - headerHeight;
    var padding = 10;
    var top = element.offsetTop;
    var height = element.offsetHeight;
    var desired = top
        - Math.max((available - height) / 2, padding)
        - headerHeight;
    var scrollTopMax = html.offsetHeight - scrollHeight;
    ScrollTo(Math.min(desired, scrollTopMax), callback);
}

function GoToMessage(messageId) {
    var element = document.getElementById("message" + messageId);
    if (element) {
        var hash = "#go_to_message" + messageId;
        if (location.hash != hash) {
            location.hash = hash;
        }
        ScrollToElement(element, function () {
            AddClass(element, "selected");
            setTimeout(function () {
                RemoveClass(element, "selected");
            }, 1000);
        });
    } else {
        ShowToast("This message was not exported. Maybe it was deleted.");
    }
    return false;
}

function GoBack(anchor) {
    if (!window.AllowBackFromHistory) {
        return true;
    }
    history.back();
    if (!anchor || !anchor.getAttribute) {
        return true;
    }
    var destination = anchor.getAttribute("href");
    if (!destination) {
        return true;
    }
    setTimeout(function () {
        location.href = destination;
    }, 100);
    return false;
}
(function () {
    var selector = 'a.photo_wrap, a.sticker_wrap, a.video_file_wrap, a.animated_wrap';
    var overlay = null, contentEl = null, prevEl = null, nextEl = null;
    var items = [], current = -1;

    function isVideo(href) {
        return /\.(mp4|webm|mov|m4v|ogv)(\?.*)?$/i.test(href);
    }

    function collectItems() {
        items = [];
        var links = document.querySelectorAll(selector);
        for (var i = 0; i < links.length; i++) {
            var href = links[i].getAttribute('href');
            if (href) {
                items.push({ href: href, caption: links[i].getAttribute('data-caption') || '' });
            }
        }
    }

    function ensureOverlay() {
        if (overlay) {
            return;
        }
        overlay = document.createElement('div');
        overlay.className = 'lightbox';
        overlay.innerHTML = '<div class="lightbox_close">&times;</div>'
            + '<div class="lightbox_prev">&#8249;</div>'
            + '<div class="lightbox_next">&#8250;</div>'
            + '<div class="lightbox_content"></div>';
        document.body.appendChild(overlay);
        contentEl = overlay.querySelector('.lightbox_content');
        prevEl = overlay.querySelector('.lightbox_prev');
        nextEl = overlay.querySelector('.lightbox_next');
        overlay.addEventListener('click', function (e) {
            if (e.target === overlay || e.target === contentEl) {
                closeLightbox();
            }
        });
        overlay.querySelector('.lightbox_close').addEventListener('click', function (e) {
            e.stopPropagation();
            closeLightbox();
        });
        prevEl.addEventListener('click', function (e) {
            e.stopPropagation();
            show(current - 1);
        });
        nextEl.addEventListener('click', function (e) {
            e.stopPropagation();
            show(current + 1);
        });
    }

    function show(i) {
        if (!items.length) {
            return;
        }
        if (i < 0) {
            i = items.length - 1;
        }
        if (i >= items.length) {
            i = 0;
        }
        current = i;
        var href = items[i].href;
        var caption = items[i].caption;
        contentEl.innerHTML = '';
        var el;
        if (isVideo(href)) {
            el = document.createElement('video');
            el.src = href;
            el.controls = true;
            el.autoplay = true;
            el.loop = true;
        } else {
            el = document.createElement('img');
            el.src = href;
        }
        el.className = 'lightbox_media';
        contentEl.appendChild(el);
        if (caption) {
            var cap = document.createElement('div');
            cap.className = 'lightbox_caption';
            cap.textContent = caption;
            contentEl.appendChild(cap);
        }
        var many = items.length > 1;
        prevEl.style.display = many ? '' : 'none';
        nextEl.style.display = many ? '' : 'none';
    }

    function openLightbox(href) {
        collectItems();
        var i = -1;
        for (var k = 0; k < items.length; k++) {
            if (items[k].href === href) {
                i = k;
                break;
            }
        }
        if (i < 0) {
            items = [{ href: href, caption: '' }];
            i = 0;
        }
        ensureOverlay();
        overlay.classList.add('shown');
        document.body.style.overflow = 'hidden';
        show(i);
    }

    function closeLightbox() {
        if (!overlay) {
            return;
        }
        overlay.classList.remove('shown');
        contentEl.innerHTML = '';
        document.body.style.overflow = '';
    }

    document.addEventListener('click', function (e) {
        var target = e.target;
        var link = null;
        while (target && target !== document.body) {
            if (target.tagName === 'A' && /(photo_wrap|sticker_wrap|video_file_wrap|animated_wrap)/.test(target.className)) {
                link = target;
                break;
            }
            target = target.parentNode;
        }
        if (!link) {
            return;
        }
        var href = link.getAttribute('href');
        if (!href) {
            return;
        }
        e.preventDefault();
        openLightbox(href);
    });

    document.addEventListener('keydown', function (e) {
        if (!overlay || !overlay.classList.contains('shown')) {
            return;
        }
        if (e.key === 'Escape' || e.keyCode === 27) {
            closeLightbox();
        } else if (e.key === 'ArrowLeft' || e.keyCode === 37) {
            show(current - 1);
        } else if (e.key === 'ArrowRight' || e.keyCode === 39) {
            show(current + 1);
        }
    });

    document.addEventListener('DOMContentLoaded', function () {
        var allLinks = document.querySelectorAll('a[href]');
        for (var li = 0; li < allLinks.length; li++) {
            var lhref = allLinks[li].getAttribute('href') || '';
            if (/^(https?:|tg:|mailto:)/i.test(lhref)) {
                allLinks[li].setAttribute('target', '_blank');
                allLinks[li].setAttribute('rel', 'noopener noreferrer');
            }
        }
    });

    document.addEventListener('DOMContentLoaded', function () {
        if (!document.querySelector('.chat_page')) {
            return;
        }
        var btn = document.createElement('div');
        btn.className = 'scroll_down_btn';
        btn.innerHTML = '<div class="scroll_down_arrow"></div>';
        document.body.appendChild(btn);
        btn.addEventListener('click', function () {
            var target = document.documentElement.scrollHeight || document.body.scrollHeight;
            try {
                window.scrollTo({ top: target, behavior: 'smooth' });
            } catch (e) {
                window.scrollTo(0, target);
            }
        });
        function updateVisibility() {
            var pos = window.scrollY || document.documentElement.scrollTop || document.body.scrollTop || 0;
            var full = document.documentElement.scrollHeight || document.body.scrollHeight;
            var maxScroll = full - window.innerHeight;
            if (maxScroll - pos > 200) {
                btn.classList.add('shown');
            } else {
                btn.classList.remove('shown');
            }
        }
        window.addEventListener('scroll', updateVisibility);
        window.addEventListener('resize', updateVisibility);
        updateVisibility();
    });
})();