.global _start
.text
_start:
 mov $13, %eax
 mov $10, %edi
 lea action(%rip), %rsi
 xor %edx, %edx
 mov $8, %r10d
 syscall
 test %eax, %eax
 jne fail
 # Block SIGUSR1 and queue it to ourselves.
 mov $14, %eax
 xor %edi, %edi
 lea mask(%rip), %rsi
 xor %edx, %edx
 syscall
 mov $39, %eax
 syscall
 mov %eax, %edi
 mov $10, %esi
 mov $62, %eax
 syscall
 cmpl $0, seen(%rip)
 jne fail
 # Unblocking delivers the handler before the following instruction.
 mov $0x123456789abcdef0, %r12
 mov $14, %eax
 mov $1, %edi
 lea mask(%rip), %rsi
 xor %edx, %edx
 syscall
 cmp $123, %eax
 jne fail
 cmpl $1, seen(%rip)
 jne fail
 mov $0x123456789abcdef0, %rax
 cmp %rax, %r12
 jne fail
 xor %edi, %edi
 jmp exit
handler:
 cmp $10, %edi
 jne fail
 cmpl $10, (%rsi)
 jne fail
 # Editing ucontext's saved RAX changes the resumed syscall result.
 movq $123, 144(%rdx)
 incq seen(%rip)
 xor %r12d, %r12d
 ret
restorer:
 mov $15, %eax
 syscall
fail:
 mov $1, %edi
exit:
 mov $231, %eax
 syscall
.data
.align 8
action: .quad handler, 0x4000004, restorer, 0
mask: .quad 512
seen: .quad 0
