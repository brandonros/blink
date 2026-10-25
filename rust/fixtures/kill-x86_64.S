.global _start
.text
_start:
 sub $16, %rsp
 mov $57, %eax
 syscall
 test %eax, %eax
 jz child
 mov %eax, %r12d
 mov %eax, %edi
 mov $9, %esi
 mov $62, %eax
 syscall
 test %eax, %eax
 jne fail
 mov %r12d, %edi
 mov %rsp, %rsi
 xor %edx, %edx
 xor %r10d, %r10d
 mov $61, %eax
 syscall
 cmp %r12d, %eax
 jne fail
 cmpl $9, (%rsp)
 jne fail
 xor %edi, %edi
 jmp exit
child:
 jmp child
fail:
 mov $1, %edi
exit:
 mov $60, %eax
 syscall
