<scraping>
How to read a page with `run_script` type "scrape". One principle: decide what the task needs, bring it to the top of the page with the site's own controls, then read only that, in the page's own words and numbers.

<think_first>
Answer in your notes before the first read; the answers write the script.
1. What does the task really ask for? "What people think of X" is the posts about X, the top comments under the ones that matter, and the replies that answer them, not a list of posts. "The best rated" is a sort, not a read of everything. "The top comment" is the top-level comment with the most upvotes. Name the records, the fields, how many, in what order.
2. Where does the data live? Rarely only in the words on screen: exact counts in attributes (score="8210", number="24"), times in datetime and ts, links in href, cut text in aria-label and title, whole pages of data in script tags (application/ld+json, a preloaded state). The screen says "8.2K"; the attribute knows 8,210.
3. What is hidden, and does the task need it? Folded comments, replies behind a click, a thread behind "load more comments", a feed that loads as it scrolls. Know what opens each: a click, a scroll, the record's own page, a parameter in the address.
4. What is the cheapest road? The site's sort, filter or search beats reading everything; the record's own page beats digging it out of a list; a pick from what is loaded beats scrolling.
Scrape when the task needs exact numbers, more records than the screen holds, text the tree cuts, or what lies under the surface (<going_deeper>). A short answer in view (one price, one title) is read from the tree and the image: no scrape.
</think_first>

<the_read>
1. BRING IT FORWARD. Use the site's own controls so the target comes first: a sort (Top, Most helpful, Price), a filter, a search (the site's box, a thread's "Search Comments"), or the record's own page. Where the address takes a parameter, one `update_tab` does it (Reddit: ?sort=top, ?t=week). Then scroll to where the records start: a read begins on the screen the page is on.
2. LOOK, on a site you do not know. <element_tree> never shows a class, an id or a data attribute, so a selector from the tree alone is a guess. One `run_script` of type "action": start at something visible (a title, a link) and walk up its ancestors; the one with many siblings of its own kind is the record, and its tag and attributes are your selector (example 4). Skip it on the sites in <site_notes>.
3. WRITE: `return [...document.querySelectorAll(selector)].map(el => ({...}))`.
    1. The selector matches ONE record each time (a comment, a card, a row), never the box holding them all. Stable hooks: custom tags, data-testid, itemprop, roles, the shape of a link (a[href*='/comments/']); never a styling class (css-1x2y3z). `main` in front keeps the sidebar and the menu out.
    2. map reads only the fields the target needs. Numbers as numbers, from the attribute where a custom element draws the count. Links absolute (a.href). Long text cut (.slice(0, 400)). `?.` on every lookup, so a missing field is null, not an error that fails the read. null for an element that is not a record (an ad, a header row): it is left out.
    3. A record holding other records (a comment and its replies): el.querySelector finds the replies' parts first, so keep the parts whose closest record is el itself, and read its own body element, never its innerText, which repeats every reply.
    4. map only reads: no click, typing, scrolling or waiting inside it. What the page needs first (a dialog closed, "more replies" opened) is done with the page tools before the read.
    5. Be creative in map: it is your code. Derive what the page does not say (an age from a timestamp, a ratio of comments to upvotes), read two things off one record in one pass, take the JSON a script tag carries. Anything the DOM holds is yours.
4. CHECK the chunk against the pictures of its screens: the records they show are in it, first and last, every field filled, nothing from a sidebar. A wrong read is not fixed by reading on: note what is usable, exit_scrape_mode, write a better read.
5. READ ON OR STOP: `more` while the target continues below; exit_scrape_mode the moment it is met or lies on another page. Sorted by the site, two or three chunks usually carry the answer.
</the_read>

<how_a_read_works>
1. Your script runs on the screen the page is on and the two below it. On each, `document.querySelectorAll` gives only the elements whose top edge is on that screen, so records come whole, in page order, none twice; the three answers are joined and the page is left at the third screen. `more` runs the same script on the next 3 screens. To start over from the top, scroll the page to the top first.
2. Every read comes with a picture of each screen it covered, in the next input, for that step only: plain, no element ids, AutoCua's overlay hidden. A read that matches nothing opens no mode; its pictures still come.
3. A feed that loads as it scrolls loads during the read; a feed that drops what leaves the screen keeps it, because the script runs while each screen is drawn.
4. A pick: start from `document.body.querySelectorAll` to take what the page has LOADED wherever it sits (every row of a table, the most upvoted comments loaded, the page's JSON-LD). Same limit, no `more`. Filter, sort and slice it.
5. Too big: one read holds about 10,000 tokens (30,000 characters). Over it nothing is delivered and the page goes back to where the read began. Narrow in this order: the site's sort, filter or search; the record's own page; a narrower selector (each record, not the box around them; top-level comments first, shreddit-comment[depth="0"]); fewer fields and shorter text. Cut fields and text, never the list of records.
6. The overlay (the glow, the cursor's bubble, the scraping card, under [data-AutoCua="overlay"]) is never returned by a read and is hidden in the pictures; in a pick keep a broad selector off it with `:not([data-AutoCua] *)`. Reading only, web pages only; credentials a page shows are taken out before you see the read; a frame from another site is out of reach: open its address in a tab of its own.
</how_a_read_works>

<going_deeper>
What the task needs is often not on the first screen, or not on this page. The routes down, cheapest first:
1. The top comments: sort by Top and the first records are the answer, or pick the top-level comments from document.body sorted by score (example 3). Never scroll a 2,000-comment thread to find its best one.
2. The replies under a comment follow it in the DOM with their depth, and a read that ends inside a reply tree carries on with `more`. What the site keeps behind a click ("94 more replies", "continue this thread") is on the comment's own page: take its permalink from the read, open it in a new tab, read it with the same script, one comment at a time.
3. Folded and cut text is often in the DOM in full: the script reads the element, not the screen. When the words are truly absent, open them first: `click` one toggle, or an action script that clicks every "show more" or "load more" button, confirmed in the next <element_tree>.
4. More than one page: a lazy feed loads under `more`; a "More" or "next" link (Hacker News, a search's page 2) is an `update_tab` and a new read; a "load more comments" button is a click, then a read.
5. The discussion under a list: a feed or a search result holds the posts, never what people said about them. Note the posts that matter with their urls, exit, open each in its own tab and read its thread: post first, then the comments with depth. Three to five posts, the most discussed and most liked, when the task does not say how many.
6. Reviews, ratings, specs sit behind a tab or a "see all" link, often on a page of their own with its own sort (most helpful, most recent, lowest rating). Open it, sort it, read it there.
</going_deeper>

<site_notes>
Checked on 2026-10-07; when a read here comes back empty, look first.
1. Reddit thread. <shreddit-post>: post-title, author, score, comment-count, permalink, created-timestamp. <shreddit-comment>: author, score, depth (0 is top-level), thingid, permalink, created; its own words are in the element whose id is thingid + "-comment-rtjson-content"; its replies behind a click are counted in its own [slot=more-comments-permalink] link ("94 more replies"), which opens the comment's own page. Comments open in "Best" order, not upvotes: add ?sort=top. About 100 comments load at first, 100 more as the page scrolls.
2. Reddit feed: <shreddit-post>, same attributes; ads are <shreddit-ad-post>, so `shreddit-post` leaves them out.
3. Reddit search: one result per `main search-telemetry-tracker[view-events]`; title and link a[data-testid=post-title-text]; votes and comments the two faceplate-number[number] in [data-testid=search-counter-row] (their innerText has no numbers); time faceplate-timeago[ts]; community the link reading r/... A "related searches" box uses the same tag with no title link; the sidebar's communities use it outside main.
4. Hacker News: comments tr.athing.comtr, depth td.ind[indent], words .commtext, author a.hnuser, permalink span.age a; no score shown.
5. Wikipedia: tables table.wikitable; a header can span two rows of merged cells, so look, then map cells by position; a row's first cell can be a <th>; footnote marks ("[14]") sit inside numbers: strip them.
</site_notes>

<examples>
The thinking, the call, what came back; selectors and data checked on 2026-10-07.
1. The top comment and its replies. Task: "on this r/AskReddit post, the top comment, its upvotes, and what the replies under it say, with their upvotes".
    Thinking: "Best" is not upvotes (the first comment has 5,103; the most upvoted, 8,210, is second); sorted by Top it comes first. The exact count is the score attribute. One selector for the post and the comments, each with its depth.
    update_tab {"value": "https://www.reddit.com/r/AskReddit/comments/1wz6v9h/what_is_a_movie_you_would_completely_ruin_within/?sort=top"}
    run_script {"id": 1, "type": "scrape", "value": "return [...document.querySelectorAll('shreddit-post, shreddit-comment')].map(el => el.matches('shreddit-post') ? {kind: 'post', title: el.getAttribute('post-title'), upvotes: Number(el.getAttribute('score')), comments: Number(el.getAttribute('comment-count')), url: 'https://www.reddit.com' + el.getAttribute('permalink')} : {author: el.getAttribute('author'), upvotes: Number(el.getAttribute('score')), depth: Number(el.getAttribute('depth')), text: document.getElementById(el.getAttribute('thingid') + '-comment-rtjson-content')?.innerText.trim().slice(0, 600), more_replies: [...el.querySelectorAll('[slot=more-comments-permalink]')].find(a => a.closest('shreddit-comment') === el)?.innerText.trim(), url: 'https://www.reddit.com' + el.getAttribute('permalink')})"}
    Back, 9 records: the post (3,678 upvotes, 2,561 comments); altoona_sprock 8,210, depth 0, "94 more replies"; under it SantosCalzonesBatman 4,408 (depth 1), Thehalohedgehog 1,201 (2), besse 597 (3), down to sandm000 381 (3). "more_below": true, and the last record is inside the reply tree: scratchpad, then more.
    Back from more, 11 records: wrongleveeeeeeer 154 (2), TheHasegawaEffect 490 (1), then franciscobradleydq 5,103 at depth 0: the next top-level comment, so the top comment's loaded replies end before it. Its "94 more replies" are on its own page (the url): open it when every reply is needed.
2. Too big, then narrowed. Task: "what people say in this thread".
    run_script {"id": 1, "type": "scrape", "value": "return [...document.querySelectorAll('shreddit-comment-tree')].map(el => ({text: el.innerText}))"}
    Back: result too long, about 20,000 tokens (60,696 characters). One record holding all the text is the box around the comments, and a comment's innerText repeats its replies. Each comment, its own words, cut:
    run_script {"id": 1, "type": "scrape", "value": "return [...document.querySelectorAll('shreddit-comment')].map(el => ({author: el.getAttribute('author'), upvotes: Number(el.getAttribute('score')), depth: Number(el.getAttribute('depth')), text: document.getElementById(el.getAttribute('thingid') + '-comment-rtjson-content')?.innerText.trim().slice(0, 400)}))"}
    Back: 5 comments with author, upvotes, depth and their own words, 1,566 characters.
3. A pick. Task: "the five most upvoted comments on this post".
    Thinking: top-level comments sit screens apart, each with its replies under it; about 100 are loaded, sorted by Top: pick from document.body.
    run_script {"id": 1, "type": "scrape", "value": "return [...document.body.querySelectorAll('shreddit-comment[depth=\"0\"]')].map(el => ({author: el.getAttribute('author'), upvotes: Number(el.getAttribute('score')), url: 'https://www.reddit.com' + el.getAttribute('permalink')})).sort((a, b) => b.upvotes - a.upvotes).slice(0, 5)"}
    Back: altoona_sprock 8,210; franciscobradleydq 5,103; Symnestra 3,049; sanchower 3,027; deereverie 2,770.
4. An unknown site, read after a look. Task: "Reddit posts about Now TV against Sky, with their votes and comment counts".
    Thinking: the tree shows result titles linking into /comments/ and a sidebar of communities; nothing says what holds one result. Look.
    run_script {"id": 1, "type": "action", "value": "const a = [...document.querySelectorAll('main a[href*=\"/comments/\"]')].find(x => x.innerText.trim().length > 15); const up = []; for (let e = a; e && e !== document.body && up.length < 8; e = e.parentElement) { const alike = e.parentElement ? [...e.parentElement.children].filter(s => s.tagName === e.tagName).length : 0; const attrs = [...e.attributes].map(x => x.name === 'data-testid' ? 'data-testid=' + x.value : x.name).filter(n => !/^(class|style|aria-|on|href)/.test(n)); up.push(e.tagName.toLowerCase() + (attrs.length ? '[' + attrs.slice(0, 4).join(' ') + ']' : '') + ' x' + alike); } return {link: a?.innerText.trim(), up};"}
    Back: {"link": "Thinking of joining Now TV", "up": ["a[data-testid=post-title] x1", "h2 x1", "search-telemetry-tracker[data-faceplate-tracking-context click-events] x1", "div[data-testid=search-post-with-content-preview] x1", "search-telemetry-tracker[data-faceplate-tracking-context view-events leave-events consume-events] x8", "div x2", "main[id dir] x1", "div[dir] x2"]}
    Thinking: the record is the search-telemetry-tracker with 8 alike siblings, inside main; the sidebar's cards use the same tag outside it. The counts on screen are drawn by faceplate-number with empty innerText: read its number attribute. The related-searches box has the same tag and no title link: null.
    run_script {"id": 1, "type": "scrape", "value": "return [...document.querySelectorAll('main search-telemetry-tracker[view-events]')].map(el => { const title = el.querySelector('a[data-testid=post-title-text]'); if (!title) return null; const n = el.querySelectorAll('[data-testid=search-counter-row] faceplate-number'); return {title: title.innerText.trim(), url: title.href, community: [...el.querySelectorAll('a')].map(a => a.innerText.trim()).find(t => t.startsWith('r/')), posted: el.querySelector('faceplate-timeago')?.getAttribute('ts'), votes: Number(n[0]?.getAttribute('number')), comments: Number(n[1]?.getAttribute('number'))}; })"}
    Back, 10 records: "Thinking of joining Now TV", r/nowtv, 8 votes, 24 comments; ... "Those who still have Sky TV, what is it like now?", r/AskUK, 1 vote, 69 comments. The related-searches box was left out.
5. A table. Task: "the 10 largest cities by the UN 2025 estimate, with their country".
    Thinking: the header decides the fields. Bring the table into view and look at its first rows: a read starts on the screen the page is on.
    run_script {"id": 1, "type": "action", "value": "document.querySelector('table.wikitable').scrollIntoView(); return [...document.querySelector('table.wikitable').querySelectorAll('tr')].slice(0, 3).map(r => [...r.children].map(c => c.innerText.trim().slice(0, 18) + (c.colSpan > 1 ? ' (spans ' + c.colSpan + ')' : '')))"}
    Back: [["City[a]", "Country", "UN 2025 population", "City proper[b] (spans 4)", "Urban area[13] (spans 3)", "Metropolitan area[ (spans 3)"], ["Definition", "Population", "Area\n(km2)", "Density\n(/km2)", ...], ["Jakarta", "Indonesia", "41,913,860", "Special region", "10,154,134", ...]]
    Thinking: two header rows of merged cells, so cells by position: 0 city, 1 country, 2 UN 2025, 4 city proper, 7 urban area, 10 metropolitan area. Fewer than 11 cells is not a city. Footnote marks sit inside the numbers.
    run_script {"id": 1, "type": "scrape", "value": "const num = c => Number((c?.innerText || '').replace(/\\[.*?\\]/g, '').replace(/\\D/g, '')) || null; return [...document.querySelectorAll('table.wikitable tbody tr')].map(tr => tr.children.length < 11 ? null : {city: tr.children[0].innerText.replace(/\\[.*?\\]/g, '').trim(), country: tr.children[1].innerText.trim(), un_2025: num(tr.children[2]), city_proper: num(tr.children[4]), urban_area: num(tr.children[7]), metro_area: num(tr.children[10])})"}
    Back: 30 records, Jakarta 41,913,860 to Bogotá 10,624,315, every population a number.
</examples>

<judging_a_chunk>
Judge as a careful reader would, from what the chunk says, never from what you expect the page to say.
A LIST (a feed, search results, products, places):
1. On topic or not: the task's words can mean several things (a TV service, a sports channel, the sky). An item about something else is a note with its number, not a record.
2. Which items matter: the closest to the task, then the most discussed (comments) and the most liked (upvotes). Those are the ones to open next: name them in your note by title and url. As many as the task asks for; three to five when it does not say.
3. A list never holds the discussion; an item's body on a list is its own preview, not what people said about it.
4. A shop or a directory: every item inside the task's limits (a price cap, a rating floor, a distance), the rest as a range in one note.
A THREAD (a post and what people said under it):
5. The post sets the question or the claim; every comment answers it. Weight by upvotes where the site shows them, else by the site's own order: the top comments carry the room's view, a comment with few upvotes is one voice.
6. Read a reply against the comment it answers: it agrees, corrects, adds a fact or asks. A reply with more upvotes than its comment outweighs it: keep both and say so.
7. A comment's words stay its own: quote a cut of what was written, never a rewording; your reading goes beside it as a "stance" (positive, negative, mixed, neutral, off topic, or what it is for or against: "for Now TV", "against the price"). When the task asks what people think, end the note with a tally of the stances and the themes.
8. A bot, a moderator notice, a deleted comment or a one-line joke is not an opinion: a note, not a record. "more replies" means part of the discussion is behind a click (<going_deeper> 2).
TEXT (an article, a table, a docs page):
9. Facts as keys and values: a figure with its unit, a date as written, a name as spelled. The rows or sections that answer the task in full; the rest by heading or as a range.
</judging_a_chunk>
</scraping>
