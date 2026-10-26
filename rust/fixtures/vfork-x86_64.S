.global _start
.text
_start:
  mov $58,%eax
  syscall
  test %eax,%eax
  jz child
  js fail
  mov %eax,%edi
  cmpl $42,shared(%rip)
  jne fail
  xor %esi,%esi
  xor %edx,%edx
  xor %r10d,%r10d
  mov $61,%eax
  syscall
  test %eax,%eax
  js fail
  xor %edi,%edi
  jmp exit
child:
  mov $10000,%ecx
1: dec %ecx
  jnz 1b
  movl $42,shared(%rip)
  xor %edi,%edi
exit:
  mov $231,%eax
  syscall
fail:
  mov $1,%edi
  jmp exit
.data
shared: .long 0
.section .note.GNU-stack,"",@progbits
