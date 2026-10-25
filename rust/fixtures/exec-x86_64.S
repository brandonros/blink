.global _start
.text
_start:
 # execve(argv[1], argv + 1, NULL)
 mov 16(%rsp), %rdi
 lea 16(%rsp), %rsi
 xor %edx, %edx
 mov $59, %eax
 syscall
 mov $1, %edi
 mov $60, %eax
 syscall
