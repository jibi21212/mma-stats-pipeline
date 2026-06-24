# VHS script for the README demo GIF. Regenerate with:  vhs docs/images/tui-demo.tape
# Records the TUI: Home -> Predict -> a matchup -> prime-vs-prime -> snapshot picker.

Output docs/images/tui-demo.gif

Set Shell "bash"
Set FontSize 15
Set Width 1080
Set Height 680
Set Padding 14
Set Theme "Dracula"
Set Framerate 12

# Boot the prebuilt TUI off-screen so the GIF starts on the app, not a shell prompt.
Hide
Type "cd /Users/jibi/Documents/Personal_Projects/mma-stats-pipeline" Enter
Type "./tui-rs/target/release/mma-tui" Enter
Sleep 6s
Show

# Home -> Predict (menu: Scrape, Database, Predict, Model)
Sleep 1500ms
Down Sleep 500ms
Down Sleep 500ms
Enter Sleep 1500ms

# Pick fighter A, then fighter B (fuzzy search -> Enter commits the top match)
Type "Israel Adesanya" Sleep 1200ms
Enter Sleep 1500ms
Type "Robert Whittaker" Sleep 1200ms
Enter Sleep 2500ms

# Show the win probability + win-by-method breakdown
Sleep 2500ms

# Prime vs prime (p) — best-self comparison + snapshot labels
Type "p" Sleep 3500ms

# Drill into a fighter's career and pick an exact past fight (s = snapshot picker)
Type "s" Sleep 2000ms
Down Sleep 600ms
Down Sleep 600ms
Down Sleep 600ms
Enter Sleep 3000ms

# Back to Home and quit cleanly
Escape Sleep 1000ms
Type "q"
Sleep 800ms
