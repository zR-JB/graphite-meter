package goclient

import (
	"context"
	"errors"
	"fmt"
	"net"
	"net/http"
	"slices"

	"github.com/coder/websocket"
	"github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// FailureReason is why a server left a run; the browser saves the same list (FAILURE_REASONS).
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
	errStalled  = errors.New("stopped delivering bytes")
	errNoBytes  = errors.New("no bytes moved")
	errProtocol = errors.New("unexpected server response")
)

// statusError is an HTTP answer that is neither success nor a sign-in request.
type statusError struct {
	code int
	from string
}

func (e statusError) Error() string { return fmt.Sprintf("HTTP %d from %s", e.code, e.from) }

func (e statusError) busy() bool {
	return e.code == http.StatusTooManyRequests || e.code == http.StatusServiceUnavailable
}

// laneRefusal ends a lane at once, except that a busy server is retried until the redial window lapses.
func laneRefusal(res *http.Response) error {
	err := unexpectedStatus(res)
	if status, ok := errors.AsType[statusError](err); ok && status.busy() {
		return err
	}
	return refusal{err}
}

type laneEnd wire.LaneEnd

func (e laneEnd) Error() string { return "the server ended the lane: " + e.Name }

// laneEnding reads a lane's close code: a revoked grant asks for sign-in, anything else is redialled.
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

// failureReason classifies err; one without evidence of its cause failed preparation or lost its connection.
func failureReason(err error, preparing bool) FailureReason {
	status, answered := errors.AsType[statusError](err)
	end, ended := errors.AsType[laneEnd](err)
	_, auth := errors.AsType[*AuthRequiredError](err)
	_, network := errors.AsType[*net.OpError](err)
	switch {
	case auth:
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
