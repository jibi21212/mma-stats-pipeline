// Package fetch provides a shared, rate-limited HTTP client that returns parsed
// goquery documents.
//
// It replaces the original Python scraper's blanket time.sleep(1) between every
// request with a token-bucket rate limiter: the limiter caps the *aggregate*
// request rate while a worker pool (see main.go) issues many requests
// concurrently, so throughput is bounded by --rate rather than by a serial
// per-page sleep.
//
// ufcstats.com now gates every page behind a lightweight JavaScript "proof of
// work" anti-bot interstitial ("Checking your browser…"): the page embeds a
// nonce and a difficulty, the browser brute-forces an n such that
// sha256("nonce:n") starts with a number of hex zeros, POSTs {nonce,n} to /__c
// to receive a clearance cookie, then reloads. GetDoc performs that same
// computation transparently (the site's own published algorithm) and keeps the
// clearance cookie in a shared jar so subsequent requests sail through. This is
// the only way any HTTP scraper — including the original Python one — can read
// the site now; it is brute-forcing the site's own challenge, not bypassing
// authentication or a paywall.
//
// All fetches are fail-soft: GetDoc returns an error that callers log and skip;
// a single bad page never aborts the run.
package fetch

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/cookiejar"
	"net/url"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/PuerkitoBio/goquery"
	"golang.org/x/time/rate"
)

// Client wraps a shared *http.Client (with a cookie jar) and a rate limiter. One
// Client is created at startup and shared across all workers; the limiter
// serializes the request rate, not the HTTP client itself (which is safe for
// concurrent use). solveMu serializes anti-bot challenge solving so a burst of
// concurrent workers hitting an expired cookie only solves it once.
type Client struct {
	httpClient *http.Client
	limiter    *rate.Limiter
	userAgent  string
	solveMu    sync.Mutex
}

// New builds a Client with a 30s request timeout (matching the Python
// requests.get(timeout=30)), a cookie jar (to hold the anti-bot clearance
// cookie), and a token-bucket limiter allowing reqPerSec requests per second.
// The burst is 1 so requests are spaced smoothly at the configured rate even
// when many workers are ready at once — this avoids slamming the server with a
// burst that trips its 429 throttle. Transient 429/503 responses are still
// retried with backoff in getBody.
func New(reqPerSec float64) *Client {
	const burst = 1
	jar, _ := cookiejar.New(nil) // never errors with a nil options arg
	return &Client{
		httpClient: &http.Client{Timeout: 30 * time.Second, Jar: jar},
		limiter:    rate.NewLimiter(rate.Limit(reqPerSec), burst),
		// A normal browser UA; the site does bot filtering and the bare Python
		// scraper (no UA) is served the challenge stub.
		userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 " +
			"(KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
	}
}

var (
	// nonce="19041bb36e439296"
	nonceRe = regexp.MustCompile(`nonce="([0-9a-fA-F]+)"`)
	// target=new Array(2+1).join('0')  -> capture the "2" (number of hex zeros)
	targetRe = regexp.MustCompile(`new Array\((\d+)\+1\)\.join\('0'\)`)
)

// maxPoWIterations bounds the brute-force search so a malformed/hostile
// challenge can never spin forever. The real difficulty is tiny (2 hex zeros,
// ~256 hashes on average); this ceiling is astronomically higher.
const maxPoWIterations = 1 << 26

// GetDoc waits on the rate limiter, performs a GET, transparently solves the
// site's JavaScript proof-of-work challenge if it is served, and returns a
// parsed goquery document. Fail-soft: any failure returns an error the caller
// logs and skips rather than panicking.
func (c *Client) GetDoc(ctx context.Context, rawURL string) (*goquery.Document, error) {
	body, err := c.getBody(ctx, rawURL)
	if err != nil {
		return nil, err
	}
	if isChallenge(body) {
		body, err = c.clearChallenge(ctx, rawURL, body)
		if err != nil {
			return nil, fmt.Errorf("anti-bot challenge for %s: %w", rawURL, err)
		}
	}
	doc, err := goquery.NewDocumentFromReader(bytes.NewReader(body))
	if err != nil {
		return nil, fmt.Errorf("parse %s: %w", rawURL, err)
	}
	return doc, nil
}

// getBody waits on the limiter, GETs rawURL, and returns the response body,
// erroring on any non-2xx status. Transient throttling (429) and unavailability
// (503) are retried a few times with backoff (honoring a numeric Retry-After
// header when present), since the server 429s aggressive bursts.
func (c *Client) getBody(ctx context.Context, rawURL string) ([]byte, error) {
	const maxAttempts = 4
	var lastErr error
	for attempt := 0; attempt < maxAttempts; attempt++ {
		if err := c.limiter.Wait(ctx); err != nil {
			return nil, fmt.Errorf("rate limiter wait for %s: %w", rawURL, err)
		}
		req, err := http.NewRequestWithContext(ctx, http.MethodGet, rawURL, nil)
		if err != nil {
			return nil, fmt.Errorf("build request for %s: %w", rawURL, err)
		}
		req.Header.Set("User-Agent", c.userAgent)

		resp, err := c.httpClient.Do(req)
		if err != nil {
			lastErr = fmt.Errorf("GET %s: %w", rawURL, err)
			if attempt == maxAttempts-1 || !sleepBackoff(ctx, attempt, 0) {
				break
			}
			continue
		}

		if resp.StatusCode == http.StatusTooManyRequests || resp.StatusCode == http.StatusServiceUnavailable {
			ra := parseRetryAfter(resp.Header.Get("Retry-After"))
			_, _ = io.Copy(io.Discard, resp.Body)
			resp.Body.Close()
			lastErr = fmt.Errorf("GET %s: status %d", rawURL, resp.StatusCode)
			if attempt == maxAttempts-1 || !sleepBackoff(ctx, attempt, ra) {
				break
			}
			continue
		}

		if resp.StatusCode < 200 || resp.StatusCode >= 300 {
			resp.Body.Close()
			return nil, fmt.Errorf("GET %s: unexpected status %d", rawURL, resp.StatusCode)
		}

		body, err := io.ReadAll(resp.Body)
		resp.Body.Close()
		if err != nil {
			return nil, fmt.Errorf("read %s: %w", rawURL, err)
		}
		return body, nil
	}
	return nil, lastErr
}

// sleepBackoff waits for an exponential backoff (1s, 2s, 4s, … capped at 10s) or
// the server-provided Retry-After override (in seconds), honoring context
// cancellation. It returns false if the context was cancelled.
func sleepBackoff(ctx context.Context, attempt int, retryAfterSec float64) bool {
	d := time.Duration(retryAfterSec * float64(time.Second))
	if d <= 0 {
		d = time.Duration(int64(time.Second) << uint(attempt))
	}
	if d > 10*time.Second {
		d = 10 * time.Second
	}
	t := time.NewTimer(d)
	defer t.Stop()
	select {
	case <-ctx.Done():
		return false
	case <-t.C:
		return true
	}
}

// parseRetryAfter parses the numeric (delta-seconds) form of a Retry-After
// header. The HTTP-date form is ignored (0), leaving the caller's backoff to
// apply.
func parseRetryAfter(v string) float64 {
	v = strings.TrimSpace(v)
	if v == "" {
		return 0
	}
	if s, err := strconv.ParseFloat(v, 64); err == nil && s >= 0 {
		return s
	}
	return 0
}

// isChallenge reports whether body is the "Checking your browser…" proof-of-work
// interstitial rather than real content.
func isChallenge(body []byte) bool {
	if bytes.Contains(body, []byte("Checking your browser")) {
		return true
	}
	// Fallback: the interstitial embeds a nonce and POSTs to /__c.
	return bytes.Contains(body, []byte("/__c")) && nonceRe.Match(body)
}

// clearChallenge solves the proof-of-work, obtains the clearance cookie, and
// re-fetches the page. solveMu serializes solving: when many concurrent workers
// hit an expired cookie at once, only the first solves it; the rest re-fetch
// after acquiring the lock and find themselves already cleared (the cookie jar
// is shared across the whole client).
func (c *Client) clearChallenge(ctx context.Context, rawURL string, _ []byte) ([]byte, error) {
	c.solveMu.Lock()
	defer c.solveMu.Unlock()

	// Re-fetch under the lock: another goroutine may have cleared it while we
	// waited, in which case we are already done.
	fresh, err := c.getBody(ctx, rawURL)
	if err != nil {
		return nil, err
	}
	if !isChallenge(fresh) {
		return fresh, nil
	}
	if err := c.solveChallenge(ctx, rawURL, fresh); err != nil {
		return nil, err
	}
	cleared, err := c.getBody(ctx, rawURL)
	if err != nil {
		return nil, err
	}
	if isChallenge(cleared) {
		return nil, errors.New("still challenged after submitting proof-of-work")
	}
	return cleared, nil
}

// solveChallenge parses the nonce and difficulty out of the interstitial,
// brute-forces n such that sha256("nonce:n") begins with the required number of
// hex zeros (exactly what the page's inline script does), and POSTs the solution
// to /__c so the server issues a clearance cookie into the shared jar.
func (c *Client) solveChallenge(ctx context.Context, rawURL string, body []byte) error {
	nm := nonceRe.FindSubmatch(body)
	if nm == nil {
		return errors.New("challenge: nonce not found")
	}
	tm := targetRe.FindSubmatch(body)
	if tm == nil {
		return errors.New("challenge: difficulty target not found")
	}
	nonce := string(nm[1])
	tlen, err := strconv.Atoi(string(tm[1]))
	if err != nil || tlen <= 0 || tlen > 8 {
		return fmt.Errorf("challenge: implausible difficulty %q", tm[1])
	}
	target := strings.Repeat("0", tlen)

	n := 0
	for {
		sum := sha256.Sum256([]byte(nonce + ":" + strconv.Itoa(n)))
		if hex.EncodeToString(sum[:])[:tlen] == target {
			break
		}
		n++
		if n > maxPoWIterations {
			return errors.New("challenge: proof-of-work exceeded iteration bound")
		}
	}

	u, err := url.Parse(rawURL)
	if err != nil {
		return fmt.Errorf("challenge: parse url: %w", err)
	}
	postURL := u.Scheme + "://" + u.Host + "/__c"
	form := "nonce=" + url.QueryEscape(nonce) + "&n=" + strconv.Itoa(n)

	if err := c.limiter.Wait(ctx); err != nil {
		return fmt.Errorf("challenge: limiter wait: %w", err)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, postURL, strings.NewReader(form))
	if err != nil {
		return fmt.Errorf("challenge: build POST: %w", err)
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	req.Header.Set("User-Agent", c.userAgent)

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return fmt.Errorf("challenge: POST /__c: %w", err)
	}
	defer resp.Body.Close()
	_, _ = io.Copy(io.Discard, resp.Body)
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return fmt.Errorf("challenge: /__c returned status %d", resp.StatusCode)
	}
	return nil
}
