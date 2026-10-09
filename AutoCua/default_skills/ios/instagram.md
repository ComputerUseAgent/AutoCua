---
name: instagram-mobile-ui
description: Map of the Instagram MOBILE app UI - the five bottom tabs (Home, Reels, DMs, Search, Profile), the stories tray and story viewer, the reel player, the DM list and chat, explore/search, the profile screen, post anatomy, and the create flow. Use this whenever a task touches the Instagram app on a phone: viewing or replying to stories, watching or liking reels, reading or sending DMs, searching for an account or hashtag, opening a profile, following someone, liking or commenting on a post, or posting anything. Also use it when a task simply says "open Instagram" or names an Instagram screen, so navigation starts from the real layout instead of a guess.
---
<ig_scope>
The Instagram MOBILE app (iOS/Android). Not the web or desktop site - their layouts differ completely.
1. This is a MAP, not a script. It tells you where things live and what a tap does; the route to any goal is still built from the screen actually in front of you.
2. Instagram ships layout changes in waves and A/B tests by account, region, and app version. When this map and the screen disagree, THE SCREEN WINS - see <layout_variance>.
</ig_scope>
<screen_zones>
Every screen in the app is three horizontal bands. Locate yourself by reading all three before acting.
1. TOP BAR: the screen's title/identity plus 1-3 icon buttons. Its contents change per tab; it is the most variable part of the app.
2. CONTENT: the scrollable middle - feed, grid, video, message list. This is where nearly every target lives.
3. BOTTOM NAV: five fixed tabs, present on all five main screens. It is your reliable anchor - if it is missing, you are NOT on a main tab (you are inside a story, a reel opened from elsewhere, a chat, a camera, or a settings screen) and you must exit before switching tabs.
</screen_zones>
<bottom_nav>
Five tabs, fixed at the bottom, in this LEFT-TO-RIGHT order:
1. HOME (house icon, far left): the main feed. Returns you to the app's start page from any other tab.
    1. Tapping Home while ALREADY on Home scrolls the feed back to the top and refreshes it. Use this to reset a long scroll instead of scrolling up manually.
2. REELS (play/film icon): the full-screen vertical video player. Opens on an autoplaying reel - see <reels_tab>.
3. DMs (message/paper-plane icon, CENTRE): direct messages. Carries an unread-count badge - see <dm_tab>.
4. SEARCH / EXPLORE (magnifying glass): search plus the recommendation grid - see <search_tab>.
5. PROFILE (your circular avatar, far right): your own account - see <profile_tab>.
6. The CREATE (+) button is NOT in the bottom bar. It sits in the TOP-LEFT corner of the Home, Reels, and Profile screens - see <create_flow>.
7. Tabs can also be reached by swiping horizontally between them, but TAP THE TAB ICON. A swipe is ambiguous - inside the feed and the story viewer the same gesture means something else entirely.
</bottom_nav>
<home_tab>
The default landing screen: the feed of accounts you follow, plus recommendations.
1. TOP BAR:
    1. Left: the CREATE (+) button and the Instagram wordmark. The wordmark may carry a chevron - tapping it opens a feed selector (Following / Favourites).
    2. Right: NOTIFICATIONS (heart icon) - likes, comments, follows, mentions. It is a separate screen, not a tab.
2. Directly under the top bar: the STORIES TRAY - see <stories_tray>.
3. Below the tray: the FEED, an infinite vertical scroll of posts - see <post_anatomy>. Sponsored posts and recommended (not-followed) accounts are interleaved and look almost identical to normal posts; read the label under the username.
4. Pull DOWN at the top of the feed to refresh.
</home_tab>
<stories_tray>
A horizontally scrolling row of circular avatars along the TOP of the home feed. It is the entry point to every story.
1. Order, LEFT to RIGHT:
    1. First position: YOUR OWN story - your avatar with a small "+" badge. Tapping it opens the camera to CREATE a story if you have none active, or plays your own story if you do. Be deliberate about which one you want.
    2. After that: the accounts you follow that have active stories, ordered by Instagram (recency + affinity), NOT alphabetically. Scroll the row sideways for more.
2. Ring state tells you what is unwatched:
    1. Bright coloured gradient ring = unwatched story.
    2. Grey / faded ring = already watched.
    3. "LIVE" label under or over the avatar = a live broadcast, not a story. Tapping it joins a live stream, which behaves nothing like a story - leave via the X.
3. Tap ANY avatar to open the story viewer at that account - see <story_viewer>.
4. An account with no active story has no ring in this tray at all; to see their profile instead, use <search_tab>.
</stories_tray>
<story_viewer>
Full-screen, TIME-DRIVEN, and the most fragile screen in the app for automation - it advances on its own.
1. LAYOUT:
    1. Very top: segment progress bars - one bar per story in that account's set; the filling bar is the current one.
    2. Top row under the bars: the poster's avatar + username + how long ago it was posted, and on the right a three-dot menu and an X to close.
    3. Middle: the story media itself, filling the screen.
    4. Bottom: a "Send message" reply field, plus a like (heart) and share icon.
2. GESTURES - the screen is divided invisibly:
    1. Tap the RIGHT side: jump to the next story segment.
    2. Tap the LEFT side: go back to the previous segment.
    3. PRESS AND HOLD anywhere: pause. Release resumes. This is how you stop the clock to read something.
    4. Swipe LEFT: skip to the NEXT account's stories. Swipe RIGHT: back to the previous account's.
    5. Swipe DOWN, or tap the X: exit and return to the feed.
3. AUTO-ADVANCE IS THE MAIN HAZARD: each photo segment plays roughly 5 seconds, then moves on by itself, and at the end of the last segment the viewer jumps to the NEXT ACCOUNT automatically. The screen you scanned can be gone before your action lands.
    1. Re-scan immediately before acting inside a story.
    2. If you need to read or extract something, press and hold to pause FIRST.
    3. If you overshoot, do not panic-tap; swipe right to return to that account and re-enter.
4. Replying to a story places the message in that account's DM thread, not in a public comment.
</story_viewer>
<reels_tab>
One full-screen vertical video at a time, autoplaying WITH SOUND, scrolling endlessly.
1. LAYOUT:
    1. The video fills the whole screen; the bottom nav still sits over the bottom edge.
    2. Right-hand rail, stacked vertically: LIKE (heart + count), COMMENT (bubble + count), SHARE/SEND (paper plane), a three-dot MENU, and at the bottom a small rotating disc for the audio track.
    3. Bottom-left: the poster's avatar + username + a Follow button, the caption (tap to expand), and the scrolling audio title.
2. GESTURES:
    1. Swipe UP: next reel. Swipe DOWN: previous reel.
    2. Single tap: pause/resume.
    3. Double tap: LIKE. Never double-tap to "make sure" a tap registered - you will silently like the reel.
3. Tapping the comment icon opens comments as a BOTTOM SHEET over the lower half of the video - see <navigation_notes>.
4. The reel keeps playing while you work, so the visible frame changes constantly. Judge success by the UI state (heart filled, count incremented, sheet open), never by the video frame.
</reels_tab>
<dm_tab>
The messages tab, in the CENTRE of the bottom nav.
1. INBOX SCREEN:
    1. Top: your username/title, and a "new message" (pencil/compose) icon on the right.
    2. Under it: a SEARCH field for finding a person or conversation.
    3. Then the NOTES row: small circular avatars with tiny text bubbles above them - short status notes, NOT stories. Tapping one opens a reply to that note.
    4. Below: the CONVERSATION LIST - avatar, name, last-message preview, timestamp, and a solid dot for unread. Tap a row to open the thread.
    5. There may be filter tabs or a "Requests" entry for messages from non-followers; unopened requests do not appear in the main list.
2. INSIDE A CHAT:
    1. Top: back arrow, the person's avatar + name (tap it to open their profile), and call/video icons.
    2. Middle: the message history, scrolling up for older messages.
    3. Bottom: the COMPOSER - camera, a text field, then mic/gallery/sticker icons. Tapping the field raises the keyboard, which covers the lower half of the screen.
3. Send is usually the arrow that REPLACES the mic icon once text is entered - the composer changes shape as you type, so re-scan after typing rather than reusing the pre-typing layout.
</dm_tab>
<search_tab>
Search plus the recommendation grid.
1. RESTING STATE: a search field at the top, and below it the EXPLORE GRID - a mosaic of recommended posts and reels, some tiles double-height. Scrolls forever.
2. Tap the search field to focus it; the grid is replaced by recent searches, and the keyboard rises.
3. After typing a query, results appear under category tabs, typically: Top / Accounts / Audio / Tags / Places / Reels.
    1. For a specific person, use ACCOUNTS and match the exact handle - display names repeat, handles do not.
    2. Verified accounts carry a blue check next to the handle.
4. Grid tile badges tell you what a tile opens: a play icon = reel, stacked squares = carousel, plain = single image.
5. This tab is the reliable route to ANY profile: search the handle, then tap the account row.
</search_tab>
<profile_tab>
Your own account (far-right tab). Another user's profile looks the same minus the owner-only controls.
1. TOP BAR: your username, often with a chevron for switching accounts; the CREATE (+) button top-left; and a hamburger/menu icon on the right that opens Settings and activity.
2. HEADER:
    1. Your avatar (with a story ring if you have an active story), display name, and the STATS ROW - posts / followers / following. Followers and following are TAPPABLE and open list screens.
    2. Bio text and link.
3. BUTTONS: on your own profile, "Edit profile" and "Share profile". On someone else's, "Follow"/"Following" and "Message" - the Follow button becomes "Following" once tapped, which is your visual confirmation.
4. HIGHLIGHTS: a horizontal row of circles under the bio - saved past stories, permanent, distinct from the live stories tray.
5. CONTENT TABS across the middle, switching the grid below: POSTS (grid) / REELS / TAGGED.
6. The grid is three columns; tiles are vertical (3:4), not square. Tap a tile to open that post.
7. A private account you do not follow shows the header but a locked, empty grid - that is expected, not a failure.
</profile_tab>
<post_anatomy>
Every feed post has the same parts, top to bottom:
1. HEADER: avatar + username (tap either to open the profile), an optional "Sponsored"/"Suggested for you" label, and a three-dot menu on the right.
2. MEDIA: image, video, or carousel. A carousel shows small dot indicators under it - swipe sideways to move through it. Video posts autoplay muted; tap to unmute.
3. ACTION ROW under the media:
    1. Left group: LIKE (heart), COMMENT (speech bubble), SHARE/SEND (paper plane).
    2. Right, on its own: SAVE (bookmark).
4. Below that: like count, then the caption (username + text, with "more" to expand), then "View all N comments", then the timestamp.
5. Double-tapping the MEDIA likes the post - the same accidental-like hazard as reels.
6. A filled/red heart means liked; an outline means not. That fill is the confirmation, not the tap itself.
</post_anatomy>
<create_flow>
1. Entry: the CREATE (+) button in the TOP-LEFT corner of Home, Reels, and Profile. It is no longer in the bottom bar - do not look for it there.
2. It opens the creation surface, from which you choose what to make: post, story, reel, or live.
3. Swiping RIGHT from the home feed also opens the camera directly.
4. Creating a story can also start from your own avatar in the stories tray (see <stories_tray>).
5. Treat every publish/share/post-now control as DESTRUCTIVE: it is public and immediate. Never tap one that the task did not explicitly ask for.
</create_flow>
<navigation_notes>
Behaviours that catch automation on this app specifically:
1. BOTTOM SHEETS: comments, share, and three-dot menus slide up over the lower part of the screen rather than opening a new page. They COVER the bottom nav and part of the content. Dismiss by swiping the sheet down or tapping the dimmed area above it - not with the back arrow.
2. THE KEYBOARD covers the bottom half whenever a text field is focused, hiding the composer's own send button and the bottom nav. Dismiss it before targeting anything low on the screen.
3. NO BOTTOM NAV = NOT ON A MAIN TAB. Exit first (back arrow top-left, X, or swipe from the left edge), then switch tabs.
4. THE SCREEN MOVES ON ITS OWN: stories auto-advance, reels autoplay and loop, feed video autoplays. A scan can be stale within seconds. Re-scan right before acting on any of these surfaces.
5. INFINITE SCROLL: the feed, explore grid, and reels never end and recycle their contents as you scroll. Element ids are re-assigned on every scan - always re-resolve targets, never reuse an id from an earlier step.
6. DOUBLE TAP IS A LIKE on posts, reels, and story media. Never repeat a tap to "make sure it landed" - verify visually instead.
7. Likes, follows, replies and shares are VISIBLE TO OTHER PEOPLE and often instant. Treat each as destructive: do only what the task asked, and confirm the state change (filled heart, "Following", the message appearing in the thread) rather than assuming.
8. Full-screen surfaces (story, reel, live) also hide the top bar; the X or a downward swipe is your way out.
</navigation_notes>
<layout_variance>
1. Instagram rolls interface changes out BY ACCOUNT, region, device, and app version. Two phones can both be current and look different.
2. Notably, older builds still show the PREVIOUS bottom bar: Home, Search, Create (+), Reels, Profile - with DMs reached from the top-right of the home screen instead of the bottom bar. If you see a "+" in the middle of the bottom bar, you are on that older layout: DMs are top-right, and Search sits second from the left.
3. So: read the bottom bar's five icons on arrival and identify which layout you are on BEFORE routing. It costs one look and prevents an entire wrong route.
4. Where this map and the screen disagree, follow the screen and note the difference.
</layout_variance>
