package fetch

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

// testChallengeHTML mirrors the real ufcstats.com interstitial: a nonce, a
// difficulty of new Array(2+1).join('0') (= two hex zeros), and a POST to /__c.
const testChallengeHTML = `<!doctype html><html><head><title>Loading…</title></head><body>
<p>Checking your browser…</p>
<noscript>This site requires JavaScript.</noscript>
<script>
var nonce="deadbeef0123abcd",
    target=new Array(2+1).join('0');
var n=0;
while(sha256(nonce+':'+n).slice(0,target.length)!==target){n++;}
var xhr=new XMLHttpRequest();xhr.open('POST',"/__c",true);
xhr.send('nonce='+encodeURIComponent(nonce)+'&n='+n);
</script></body></html>`

func TestIsChallenge(t *testing.T) {
	if !isChallenge([]byte(testChallengeHTML)) {
		t.Fatal("interstitial should be detected as a challenge")
	}
	if isChallenge([]byte(`<html><body><table class="b-statistics__table-row">Jon Jones</table></body></html>`)) {
		t.Fatal("real content must not be flagged as a challenge")
	}
}

// TestGetDocSolvesChallenge stands up a local server that behaves like the real
// site: it serves the interstitial until a valid proof-of-work is POSTed to
// /__c, at which point it issues a clearance cookie and serves real content.
func TestGetDocSolvesChallenge(t *testing.T) {
	var posts int
	mux := http.NewServeMux()
	mux.HandleFunc("/page", func(w http.ResponseWriter, r *http.Request) {
		if ck, err := r.Cookie("clearance"); err == nil && ck.Value == "ok" {
			_, _ = w.Write([]byte(`<html><body><table class="b-statistics__table-row">REAL DATA</table></body></html>`))
			return
		}
		_, _ = w.Write([]byte(testChallengeHTML))
	})
	mux.HandleFunc("/__c", func(w http.ResponseWriter, r *http.Request) {
		posts++
		_ = r.ParseForm()
		nonce := r.FormValue("nonce")
		n := r.FormValue("n")
		sum := sha256.Sum256([]byte(nonce + ":" + n))
		if nonce == "deadbeef0123abcd" && strings.HasPrefix(hex.EncodeToString(sum[:]), "00") {
			http.SetCookie(w, &http.Cookie{Name: "clearance", Value: "ok", Path: "/"})
			w.WriteHeader(http.StatusOK)
			return
		}
		w.WriteHeader(http.StatusForbidden)
	})
	srv := httptest.NewServer(mux)
	defer srv.Close()

	c := New(1000) // effectively unthrottled for the test

	doc, err := c.GetDoc(context.Background(), srv.URL+"/page")
	if err != nil {
		t.Fatalf("GetDoc returned error: %v", err)
	}
	if !strings.Contains(doc.Text(), "REAL DATA") {
		t.Fatalf("expected real content after solving the challenge, got: %q", doc.Text())
	}
	if posts != 1 {
		t.Fatalf("expected exactly one /__c solve, got %d", posts)
	}

	// The clearance cookie must be reused: a second fetch needs no new solve.
	if _, err := c.GetDoc(context.Background(), srv.URL+"/page"); err != nil {
		t.Fatalf("second GetDoc failed: %v", err)
	}
	if posts != 1 {
		t.Fatalf("clearance cookie not reused: /__c was hit %d times", posts)
	}
}
