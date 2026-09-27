package goclient

import (
	"context"
	"errors"
	"fmt"
	"net"
	"net/http"
	"slices"
	"time"

	"github.com/coder/websocket"
	"github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

type FailureReason string

const (
	FailurePreparation          FailureReason = "preparation-failed"
	FailureConnectionLost       FailureReason = "connection-lost"
	FailureTimeout              FailureReason = "timeout"
	FailureSignIn               FailureReason = "sign-in-required"
	FailureServerBusy           FailureReason = "server-busy"
	FailureProtocol             FailureReason = "protocol-error"
	FailureInsufficientEvidence FailureReason = "insufficient-evidence"
)

var (
	errStalled       = errors.New("stopped delivering bytes")
	errNoBytes       = errors.New("no bytes moved")
	errProtocol      = errors.New("unexpected server response")
	errUploadInvalid = errors.New("unknown upload id")
)

type statusError struct {
	code       int
	from       string
	retryAfter time.Duration
}

func (e statusError) Error() string { return fmt.Sprintf("HTTP %d from %s", e.code, e.from) }

func (e statusError) busy() bool {
	return e.code == http.StatusTooManyRequests || e.code == http.StatusServiceUnavailable
}

func laneRefusal(res *http.Response) error {
	err := unexpectedStatus(res)
	if status, ok := errors.AsType[statusError](err); ok && status.busy() {
		return err
	}
	return refusal{err}
}

// uploadRefusal acts on a refusal code (api/uploadrefusals.txt) like the browser, before the status.
func uploadRefusal(code string, status statusError) error {
	switch code {
	case "invalid":
		return refusal{fmt.Errorf("%w: %w", errUploadInvalid, status)}
	case "ownerMismatch":
		return refusal{fmt.Errorf("%w: upload id belongs to another client: %w", errProtocol, status)}
	case "globalFull":
		status.code = http.StatusServiceUnavailable
	case "clientFull":
		status.code = http.StatusTooManyRequests
	case "idle":
		return laneEnd(wire.LaneIdle)
	case "revoked":
		return &AuthRequiredError{}
	default:
		if status.code == 0 {
			return refusal{fmt.Errorf("%w: upload refused (%q) by %s", errProtocol, code, status.from)}
		}
	}
	return status
}

type laneEnd wire.LaneEnd

func (e laneEnd) Error() string { return "the server ended the lane: " + e.Name }

func laneEnding(err error) error {
	i := slices.IndexFunc(wire.LaneEnds, func(e wire.LaneEnd) bool { return e.WS == int(websocket.CloseStatus(err)) })
	if closed, ok := errors.AsType[*webtransport.SessionError](err); ok && closed.Remote {
		i = slices.IndexFunc(wire.LaneEnds, func(e wire.LaneEnd) bool { return e.WT == uint32(closed.ErrorCode) })
	}
	switch {
	case i < 0 || wire.LaneEnds[i] == wire.LaneFinished:
		return err
	case wire.LaneEnds[i] == wire.LaneRevoked:
		return &AuthRequiredError{}
	}
	return laneEnd(wire.LaneEnds[i])
}

func ReasonOf(err error) FailureReason { return failureReason(err, false) }

func failureReason(err error, preparing bool) FailureReason {
	status, answered := errors.AsType[statusError](err)
	end, ended := errors.AsType[laneEnd](err)
	_, network := errors.AsType[*net.OpError](err)
	switch {
	case IsAuthRequired(err):
		return FailureSignIn
	case answered && status.busy():
		return FailureServerBusy
	case ended && end.Name != wire.LaneShutdown.Name, errors.Is(err, errStalled),
		errors.Is(err, context.DeadlineExceeded):
		return FailureTimeout
	case errors.Is(err, errInsufficientEvidence), errors.Is(err, errNoBytes):
		return FailureInsufficientEvidence
	case network || ended:
		return FailureConnectionLost
	case answered, errors.Is(err, errProtocol):
		return FailureProtocol
	case preparing:
		return FailurePreparation
	}
	return FailureConnectionLost
}
