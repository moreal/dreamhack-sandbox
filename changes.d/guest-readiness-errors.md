---
priority: 20
---
 -  When a guest never becomes reachable, the error now says what was
    actually missing -- the SSH service, key authentication, or the shell
    itself -- along with how long it waited, the timeout in force and how to
    raise it. A guest whose SSH service has not started is no longer reported
    as ready.
