/* SPDX-License-Identifier: LGPL-2.0-or-later */

/* Include the actual implementation; only receive and PID observations are fake.
 * The focused Linux link discards the unused network-setup functions. */
#include <limits.h>
#include <stdint.h>
#include <sys/socket.h>
#include <unistd.h>

static ssize_t fake_recv (int fd, void *buffer, size_t length, int flags);
static pid_t fake_getpid (void);

#define recv fake_recv
#define getpid fake_getpid
#ifndef NETWORK_SOURCE
#define NETWORK_SOURCE "../../vendor/bubblewrap/network.c"
#endif
#include NETWORK_SOURCE
#undef getpid
#undef recv

#define TEST_FD 42
#define TEST_PID 4242
#define TEST_SEQUENCE 7

enum reply_shape
{
  ACK,
  EMPTY_DATAGRAM,
  SHORT_HEADER,
  ZERO_MESSAGE_LENGTH,
  SHORT_MESSAGE_LENGTH,
  LONG_MESSAGE_LENGTH,
  HUGE_MESSAGE_LENGTH,
  SHORT_ERROR,
  ERROR_WITHOUT_REQUEST_HEADER,
  OVERSIZED_DATAGRAM,
  WRONG_SEQUENCE,
  WRONG_PID,
  DONE,
  SHORT_DONE,
  NOOP_THEN_ACK,
  NOOP_THEN_SHORT_HEADER,
  UNPADDED_NOOP_THEN_ACK,
  PARTIAL_PADDING,
  RECEIVE_ERROR,
  INTERRUPTED_ACK,
  INTERRUPTED_ERROR,
};

struct test_case
{
  const char *name;
  enum reply_shape shape;
  int ack_error;
  int result;
  int error;
  unsigned int receives;
};

static const struct test_case cases[] = {
  { "ack-zero", ACK, 0, 0, EPERM, 1 },
  { "ack-eperm", ACK, -EPERM, -1, EPERM, 1 },
  { "ack-eacces", ACK, -EACCES, -1, EACCES, 1 },
  { "ack-eexist", ACK, -EEXIST, -1, EEXIST, 1 },
  { "ack-lowest-valid-error", ACK, -4095, -1, 4095, 1 },
  { "ack-positive-error", ACK, EPERM, -1, EPROTO, 1 },
  { "ack-int-max", ACK, INT_MAX, -1, EPROTO, 1 },
  { "ack-int-min", ACK, INT_MIN, -1, EPROTO, 1 },
  { "ack-error-below-range", ACK, -4096, -1, EPROTO, 1 },
  { "empty-datagram", EMPTY_DATAGRAM, 0, -1, EPROTO, 1 },
  { "short-header", SHORT_HEADER, 0, -1, EPROTO, 1 },
  { "zero-message-length", ZERO_MESSAGE_LENGTH, 0, -1, EPROTO, 1 },
  { "short-message-length", SHORT_MESSAGE_LENGTH, 0, -1, EPROTO, 1 },
  { "message-exceeds-datagram", LONG_MESSAGE_LENGTH, 0, -1, EPROTO, 1 },
  { "message-length-uint32-max", HUGE_MESSAGE_LENGTH, 0, -1, EPROTO, 1 },
  { "short-error", SHORT_ERROR, 0, -1, EPROTO, 1 },
  { "error-without-request-header", ERROR_WITHOUT_REQUEST_HEADER, 0, -1, EPROTO, 1 },
  { "oversized-datagram-with-zero-ack-prefix", OVERSIZED_DATAGRAM, 0, -1, EPROTO, 1 },
  { "wrong-sequence", WRONG_SEQUENCE, 0, -1, EPROTO, 1 },
  { "wrong-pid", WRONG_PID, 0, -1, EPROTO, 1 },
  { "done", DONE, 0, 0, EPERM, 1 },
  { "short-done", SHORT_DONE, 0, -1, EPROTO, 1 },
  { "noop-then-ack", NOOP_THEN_ACK, -EACCES, -1, EACCES, 1 },
  { "noop-then-short-header", NOOP_THEN_SHORT_HEADER, 0, -1, EPROTO, 1 },
  { "unpadded-noop-then-ack", UNPADDED_NOOP_THEN_ACK, -EACCES, -1, EACCES, 2 },
  { "partial-padding", PARTIAL_PADDING, 0, -1, EPROTO, 1 },
  { "receive-error", RECEIVE_ERROR, 0, -1, EAGAIN, 1 },
  { "interrupted-ack", INTERRUPTED_ACK, 0, 0, EINTR, 2 },
  { "interrupted-error", INTERRUPTED_ERROR, 0, -1, EAGAIN, 2 },
};

static struct
{
  unsigned char data[1100];
  size_t length;
  int error;
} datagrams[2];
static unsigned int datagram_count;
static unsigned int receive_calls;

static pid_t
fake_getpid (void)
{
  return TEST_PID;
}

static ssize_t
fake_recv (int fd, void *buffer, size_t length, int flags)
{
  unsigned int index = receive_calls++;
  size_t copied;

  if (fd != TEST_FD || length != 1024 || (flags != 0 && flags != MSG_TRUNC))
    {
      fprintf (stderr, "unexpected receive arguments\n");
      exit (EXIT_FAILURE);
    }
  if (index >= datagram_count)
    {
      errno = EIO;
      return -1;
    }
  if (datagrams[index].error != 0)
    {
      errno = datagrams[index].error;
      return -1;
    }

  copied = datagrams[index].length < length ? datagrams[index].length : length;
  memcpy (buffer, datagrams[index].data, copied);
  return flags & MSG_TRUNC ? (ssize_t) datagrams[index].length : (ssize_t) copied;
}

static void
prepare_reply (const struct test_case *test)
{
  struct
  {
    struct nlmsghdr header;
    struct nlmsgerr error;
  } message = {
    .header = {
      .nlmsg_len = NLMSG_LENGTH (sizeof (struct nlmsgerr)),
      .nlmsg_type = NLMSG_ERROR,
      .nlmsg_flags = NLM_F_CAPPED,
      .nlmsg_seq = TEST_SEQUENCE,
      .nlmsg_pid = TEST_PID,
    },
    .error = {
      .error = test->ack_error,
      .msg = {
        .nlmsg_len = NLMSG_LENGTH (sizeof (struct ifaddrmsg)),
        .nlmsg_type = RTM_NEWADDR,
        .nlmsg_flags = NLM_F_REQUEST | NLM_F_ACK,
        .nlmsg_seq = TEST_SEQUENCE,
        .nlmsg_pid = TEST_PID,
      },
    },
  };
  struct nlmsghdr noop = message.header;

  memset (datagrams, 0, sizeof (datagrams));
  datagram_count = 1;
  receive_calls = 0;
  datagrams[0].length = sizeof (message);
  noop.nlmsg_type = NLMSG_NOOP;
  noop.nlmsg_flags = 0;
  noop.nlmsg_len = NLMSG_HDRLEN;

  switch (test->shape)
    {
    case ACK:
    case INTERRUPTED_ACK:
    case INTERRUPTED_ERROR:
      break;
    case EMPTY_DATAGRAM:
      datagrams[0].length = 0;
      break;
    case SHORT_HEADER:
      datagrams[0].length = NLMSG_HDRLEN - 1;
      break;
    case ZERO_MESSAGE_LENGTH:
      message.header.nlmsg_len = 0;
      break;
    case SHORT_MESSAGE_LENGTH:
      message.header.nlmsg_len = NLMSG_HDRLEN - 1;
      break;
    case LONG_MESSAGE_LENGTH:
      message.header.nlmsg_len = sizeof (message) + 1;
      break;
    case HUGE_MESSAGE_LENGTH:
      message.header.nlmsg_len = UINT32_MAX;
      break;
    case SHORT_ERROR:
      message.header.nlmsg_len = NLMSG_HDRLEN + sizeof (message.error.error) - 1;
      datagrams[0].length = message.header.nlmsg_len;
      break;
    case ERROR_WITHOUT_REQUEST_HEADER:
      message.header.nlmsg_len = NLMSG_LENGTH (sizeof (message.error.error));
      datagrams[0].length = message.header.nlmsg_len;
      break;
    case OVERSIZED_DATAGRAM:
      datagrams[0].length = sizeof (datagrams[0].data);
      break;
    case WRONG_SEQUENCE:
      message.header.nlmsg_seq++;
      break;
    case WRONG_PID:
      message.header.nlmsg_pid++;
      break;
    case DONE:
      message.header.nlmsg_type = NLMSG_DONE;
      message.header.nlmsg_flags = 0;
      message.header.nlmsg_len = NLMSG_HDRLEN;
      datagrams[0].length = NLMSG_HDRLEN;
      break;
    case SHORT_DONE:
      message.header.nlmsg_type = NLMSG_DONE;
      message.header.nlmsg_flags = 0;
      datagrams[0].length = NLMSG_HDRLEN;
      break;
    case NOOP_THEN_ACK:
    case NOOP_THEN_SHORT_HEADER:
      memcpy (datagrams[0].data, &noop, sizeof (noop));
      memcpy (datagrams[0].data + NLMSG_HDRLEN, &message, sizeof (message));
      datagrams[0].length += NLMSG_HDRLEN;
      if (test->shape == NOOP_THEN_SHORT_HEADER)
        datagrams[0].length = 2 * NLMSG_HDRLEN - 1;
      return;
    case UNPADDED_NOOP_THEN_ACK:
    case PARTIAL_PADDING:
      noop.nlmsg_len = NLMSG_LENGTH (1);
      memcpy (datagrams[0].data, &noop, sizeof (noop));
      datagrams[0].length = noop.nlmsg_len;
      if (test->shape == PARTIAL_PADDING)
        datagrams[0].length++;
      else
        {
          datagram_count = 2;
          memcpy (datagrams[1].data, &message, sizeof (message));
          datagrams[1].length = sizeof (message);
        }
      return;
    case RECEIVE_ERROR:
      datagrams[0].error = EAGAIN;
      break;
    }

  memcpy (datagrams[0].data, &message, sizeof (message));
  if (test->shape == INTERRUPTED_ACK || test->shape == INTERRUPTED_ERROR)
    {
      datagrams[1] = datagrams[0];
      datagram_count = 2;
      datagrams[0].error = EINTR;
      if (test->shape == INTERRUPTED_ERROR)
        datagrams[1].error = EAGAIN;
    }
}

int
main (int argc, char **argv)
{
  unsigned int i, selected = 0, failures = 0;

  if (argc > 2)
    return 2;
  for (i = 0; i < N_ELEMENTS (cases); i++)
    {
      const struct test_case *test = &cases[i];
      int result, error;
      bool passed;

      if (argc == 2 && strcmp (argv[1], test->name) != 0)
        continue;
      prepare_reply (test);
      errno = EPERM;
      result = rtnl_read_reply (TEST_FD, TEST_SEQUENCE);
      error = errno;
      passed = result == test->result && error == test->error && receive_calls == test->receives;
      printf ("%s %u - %s: result=%d errno=%d receives=%u\n",
              passed ? "ok" : "not ok", ++selected, test->name, result, error, receive_calls);
      if (!passed)
        failures++;
    }
  printf ("1..%u\n", selected);
  if (selected == 0)
    return 2;
  return failures == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}
