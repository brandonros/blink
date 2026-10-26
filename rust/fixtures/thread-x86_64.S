.global _start
.text
_start:
 lea pipefds(%rip), %rdi
 mov $22, %eax
 syscall
 test %eax, %eax
 jne fail
 mov $0x1002, %edi
 lea parent_tls(%rip), %rsi
 mov $158, %eax
 syscall
 mov $0x7d0f00, %edi
 lea child_stack_end(%rip), %rsi
 lea tid(%rip), %rdx
 lea tid(%rip), %r10
 lea child_tls(%rip), %r8
 mov $56, %eax
 syscall
 test %eax, %eax
 jz child
wait:
 mov tid(%rip), %edx
 test %edx, %edx
 jz done
 lea tid(%rip), %rdi
 xor %esi, %esi
 xor %r10d, %r10d
 mov $202, %eax
 syscall
 jmp wait
done:
 mov %fs:0, %eax
 cmp $84, %eax
 jne fail
 cmpl $42, shared(%rip)
 jne fail
 # CLONE_FILES makes a close in the child visible to the parent.
 mov pipefds(%rip), %edi
 mov $3, %eax
 syscall
 cmp $-9, %eax
 jne fail
 xor %edi, %edi
 jmp group_exit
child:
 mov %fs:0, %eax
 cmp $42, %eax
 jne fail
 mov %eax, shared(%rip)
 mov pipefds(%rip), %edi
 mov $3, %eax
 syscall
 xor %edi, %edi
 mov $60, %eax
 syscall
fail:
 mov $1, %edi
group_exit:
 mov $231, %eax
 syscall
.bss
.align 8
pipefds: .skip 8
tid: .skip 4
shared: .skip 4
.align 16
child_stack: .skip 4096
child_stack_end:

.data
parent_tls: .quad 84
child_tls: .quad 42
