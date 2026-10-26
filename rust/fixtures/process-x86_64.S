.global _start
.text
_start:
 sub $32, %rsp
 # Create a pipe, then fork: both processes initially inherit both ends.
 mov %rsp, %rdi
 mov $22, %eax
 syscall
 test %eax, %eax
 jne fail
 mov $57, %eax
 syscall
 test %eax, %eax
 jz child
 mov %eax, %r12d
 # Parent closes its write end and reads the child's message.
 mov 4(%rsp), %edi
 mov $3, %eax
 syscall
 mov (%rsp), %edi
 lea 16(%rsp), %rsi
 mov $6, %edx
 xor %eax, %eax
 syscall
 cmp $6, %eax
 jne fail
 mov $1, %edi
 mov $1, %eax
 syscall
 # wait4 takes signed 32-bit pid_t; glibc zero-extends -1 in EDI.
 mov $-1, %edi
 lea 8(%rsp), %rsi
 xor %edx, %edx
 xor %r10d, %r10d
 mov $61, %eax
 syscall
 cmp %r12d, %eax
 jne fail
 cmpl $10752, 8(%rsp)
 jne fail
 # EOF proves the child's descriptor copies were closed on exit.
 mov (%rsp), %edi
 lea 16(%rsp), %rsi
 mov $1, %edx
 xor %eax, %eax
 syscall
 test %eax, %eax
 jne fail
 xor %edi, %edi
 jmp exit
child:
 mov (%rsp), %edi
 mov $3, %eax
 syscall
 mov 4(%rsp), %edi
 lea message(%rip), %rsi
 mov $6, %edx
 mov $1, %eax
 syscall
 mov $42, %edi
 jmp exit
fail:
 mov $1, %edi
exit:
 mov $60, %eax
 syscall
.section .rodata
message: .ascii "child\n"
